#pragma once

#include <QByteArray>
#include <QLocalSocket>
#include <QObject>
#include <QString>
#include <QStringList>
#include <QtQml/qqmlregistration.h>

// Spike client for the traytray protocol framing: newline-delimited JSON over
// a Unix socket. It keeps only the latest frame; a real shell would route
// frames by type.
class TrayClient : public QObject
{
    Q_OBJECT
    QML_ELEMENT

    // Absolute path, or a bare file name resolved against $XDG_RUNTIME_DIR.
    Q_PROPERTY(QString socketPath READ socketPath WRITE setSocketPath NOTIFY socketPathChanged)
    Q_PROPERTY(bool connected READ isConnected NOTIFY connectedChanged)
    Q_PROPERTY(QString latestFrame READ latestFrame NOTIFY latestFrameChanged)
    Q_PROPERTY(int framesReceived READ framesReceived NOTIFY latestFrameChanged)
    Q_PROPERTY(int framesRejected READ framesRejected NOTIFY framesRejectedChanged)
    Q_PROPERTY(QString lastError READ lastError NOTIFY lastErrorChanged)
    // High-water mark of the partial-line buffer, exposed so a test can show
    // that an oversized line was never held in memory.
    Q_PROPERTY(qint64 maxBufferedBytes READ maxBufferedBytes NOTIFY maxBufferedBytesChanged)

public:
    // Protocol limit from docs/design.md: a frame is at most 256 KB.
    static constexpr qint64 MaxFrameBytes = 256 * 1024;

    explicit TrayClient(QObject *parent = nullptr);

    QString socketPath() const { return m_socketPath; }
    void setSocketPath(const QString &path);
    bool isConnected() const;
    QString latestFrame() const { return m_latestFrame; }
    int framesReceived() const { return m_framesReceived; }
    int framesRejected() const { return m_framesRejected; }
    QString lastError() const { return m_lastError; }
    qint64 maxBufferedBytes() const { return m_maxBuffered; }

    Q_INVOKABLE void connectToHost();
    Q_INVOKABLE void disconnectFromHost();
    // Sends one frame. The text must parse as a JSON object; it is re-encoded
    // compactly so it cannot contain a raw newline.
    Q_INVOKABLE bool send(const QString &json);
    // Diagnostic for the spike: the import path list of the engine that
    // instantiated this object, to see where the hosting shell looks for modules.
    Q_INVOKABLE QStringList engineImportPaths() const;

Q_SIGNALS:
    void socketPathChanged();
    void connectedChanged();
    void latestFrameChanged();
    void framesRejectedChanged();
    void lastErrorChanged();
    void maxBufferedBytesChanged();
    void frameReceived(const QString &frame);

private:
    void onReadyRead();
    void consume(const char *data, qint64 size);
    void finishLine();
    void reject(const QString &reason);
    void setError(const QString &error);
    QString resolvedPath() const;

    QLocalSocket m_socket;
    QString m_socketPath;
    QByteArray m_line;
    // True while skipping the remainder of an oversized line up to its newline.
    bool m_discarding = false;
    QString m_latestFrame;
    int m_framesReceived = 0;
    int m_framesRejected = 0;
    QString m_lastError;
    qint64 m_maxBuffered = 0;
};
