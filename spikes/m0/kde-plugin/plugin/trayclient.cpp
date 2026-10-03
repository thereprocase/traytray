#include "trayclient.h"

#include <QDir>
#include <QJsonDocument>
#include <QJsonParseError>
#include <QQmlEngine>
#include <QStandardPaths>

#include <cstring>

namespace {
// Read in bounded chunks so a peer that streams megabytes without a newline
// never causes a large allocation.
constexpr qint64 ReadChunkBytes = 64 * 1024;
}

TrayClient::TrayClient(QObject *parent)
    : QObject(parent)
{
    connect(&m_socket, &QLocalSocket::readyRead, this, &TrayClient::onReadyRead);
    connect(&m_socket, &QLocalSocket::connected, this, &TrayClient::connectedChanged);
    connect(&m_socket, &QLocalSocket::disconnected, this, [this] {
        m_line.clear();
        m_discarding = false;
        Q_EMIT connectedChanged();
    });
    connect(&m_socket, &QLocalSocket::errorOccurred, this, [this](QLocalSocket::LocalSocketError) {
        setError(m_socket.errorString());
    });
}

void TrayClient::setSocketPath(const QString &path)
{
    if (path == m_socketPath)
        return;
    m_socketPath = path;
    Q_EMIT socketPathChanged();
}

bool TrayClient::isConnected() const
{
    return m_socket.state() == QLocalSocket::ConnectedState;
}

QString TrayClient::resolvedPath() const
{
    if (QDir::isAbsolutePath(m_socketPath))
        return m_socketPath;
    const QString runtimeDir = QStandardPaths::writableLocation(QStandardPaths::RuntimeLocation);
    return runtimeDir + QLatin1Char('/') + m_socketPath;
}

void TrayClient::connectToHost()
{
    if (m_socket.state() != QLocalSocket::UnconnectedState)
        m_socket.abort();
    m_line.clear();
    m_discarding = false;
    m_socket.connectToServer(resolvedPath());
}

void TrayClient::disconnectFromHost()
{
    m_socket.disconnectFromServer();
}

bool TrayClient::send(const QString &json)
{
    QJsonParseError err{};
    const QJsonDocument doc = QJsonDocument::fromJson(json.toUtf8(), &err);
    if (err.error != QJsonParseError::NoError || !doc.isObject()) {
        setError(QStringLiteral("send: not a JSON object"));
        return false;
    }
    QByteArray frame = doc.toJson(QJsonDocument::Compact);
    if (frame.size() > MaxFrameBytes) {
        setError(QStringLiteral("send: frame exceeds limit"));
        return false;
    }
    if (!isConnected()) {
        setError(QStringLiteral("send: not connected"));
        return false;
    }
    frame.append('\n');
    return m_socket.write(frame) == frame.size();
}

QStringList TrayClient::engineImportPaths() const
{
    const QQmlEngine *engine = qmlEngine(this);
    return engine ? engine->importPathList() : QStringList{};
}

void TrayClient::onReadyRead()
{
    char chunk[ReadChunkBytes];
    while (m_socket.bytesAvailable() > 0) {
        const qint64 n = m_socket.read(chunk, sizeof chunk);
        if (n <= 0)
            break;
        consume(chunk, n);
    }
}

void TrayClient::consume(const char *data, qint64 size)
{
    qint64 pos = 0;
    while (pos < size) {
        const void *nl = std::memchr(data + pos, '\n', size_t(size - pos));
        const qint64 end = nl ? static_cast<const char *>(nl) - data : size;
        const qint64 segment = end - pos;

        if (m_discarding) {
            // Still inside an oversized line: drop bytes until its newline.
            if (nl)
                m_discarding = false;
        } else if (m_line.size() + segment > MaxFrameBytes) {
            // Reject as soon as the limit is crossed, before appending, so the
            // buffer never holds more than MaxFrameBytes.
            m_line.clear();
            reject(QStringLiteral("frame exceeds %1 bytes").arg(MaxFrameBytes));
            m_discarding = !nl;
        } else {
            m_line.append(data + pos, segment);
            if (m_line.size() > m_maxBuffered) {
                m_maxBuffered = m_line.size();
                Q_EMIT maxBufferedBytesChanged();
            }
            if (nl)
                finishLine();
        }
        pos = nl ? end + 1 : size;
    }
}

void TrayClient::finishLine()
{
    const QByteArray line = m_line;
    m_line.clear();
    if (line.isEmpty())
        return;
    QJsonParseError err{};
    const QJsonDocument doc = QJsonDocument::fromJson(line, &err);
    if (err.error != QJsonParseError::NoError || !doc.isObject()) {
        reject(QStringLiteral("frame is not a JSON object"));
        return;
    }
    m_latestFrame = QString::fromUtf8(doc.toJson(QJsonDocument::Compact));
    ++m_framesReceived;
    Q_EMIT latestFrameChanged();
    Q_EMIT frameReceived(m_latestFrame);
}

void TrayClient::reject(const QString &reason)
{
    ++m_framesRejected;
    Q_EMIT framesRejectedChanged();
    setError(reason);
}

void TrayClient::setError(const QString &error)
{
    m_lastError = error;
    Q_EMIT lastErrorChanged();
}
