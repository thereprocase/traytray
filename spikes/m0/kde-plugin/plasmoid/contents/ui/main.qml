import QtQuick
import QtQuick.Layouts
import org.kde.plasma.plasmoid
import org.kde.plasma.components as PlasmaComponents
import org.traytray.spike

PlasmoidItem {
    id: root

    preferredRepresentation: fullRepresentation

    TrayClient {
        id: client
        socketPath: "traytray-spike-m0.sock"

        onConnectedChanged: {
            console.log("traytray-spike: connected =", connected)
            if (connected) {
                // Proves the QML -> socket direction.
                send(JSON.stringify({ type: "hello", proto_version: 0, app_id: "spike-shell", role: "shell" }))
            }
        }
        onFrameReceived: frame => {
            // Log the length rather than the whole frame; boundary test frames are 256 KB.
            console.log("traytray-spike: frame", framesReceived, "len", frame.length,
                        "head", frame.substring(0, 80))
            if (framesReceived % 5 === 0) {
                send(JSON.stringify({ type: "action", app: "spike-server", action_id: "ack", item_id: "frame", rev: framesReceived }))
            }
        }
        onFramesRejectedChanged: console.log("traytray-spike: rejected =", framesRejected,
                                             "error =", lastError, "maxBuffered =", maxBufferedBytes)
        onLastErrorChanged: console.log("traytray-spike: lastError =", lastError)
    }

    Component.onCompleted: console.log("traytray-spike: engine import paths =",
                                       JSON.stringify(client.engineImportPaths()))

    // Retry until the test server is up; the spike has no other reconnect logic.
    Timer {
        interval: 1000
        running: !client.connected
        repeat: true
        triggeredOnStart: true
        onTriggered: client.connectToHost()
    }

    fullRepresentation: ColumnLayout {
        Layout.minimumWidth: 420
        Layout.minimumHeight: 160

        PlasmaComponents.Label {
            text: client.connected ? "Connected" : "Not connected"
        }
        PlasmaComponents.Label {
            text: "Frames: " + client.framesReceived + "  Rejected: " + client.framesRejected
                  + "  Max buffered: " + client.maxBufferedBytes
        }
        PlasmaComponents.Label {
            id: frameText
            Layout.fillWidth: true
            Layout.maximumWidth: 600
            wrapMode: Text.WrapAnywhere
            maximumLineCount: 4
            elide: Text.ElideRight
            text: client.latestFrame.length > 200 ? client.latestFrame.substring(0, 200) + "…" : client.latestFrame
        }
    }
}
