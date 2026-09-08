import QtQuick
import Quickshell
import Quickshell.Io
import ".."

// Drop-in replacement for the hyprwhspr Quickshell service, backed by Duskr.
//
// Installed by `duskr migrate hyprwhspr` (the original is backed up first).
// It keeps the file name, the `controller.hyprwhsprService` registration and
// the property API, so HyprwhsprIndicator, HyprwhsprWaveform,
// DictationIslandContent and MorphOverlay need no changes.
Item {
    id: service
    visible: false
    required property var controller

    property bool available: false
    property string state: "stopped"
    property string tooltip: "Duskr is unavailable"
    property real level: 0
    property bool levelActive: false

    // Extras the hyprwhspr service did not have; safe for widgets to ignore.
    property bool ready: false
    property string backend: ""
    property string model: ""

    function perform(action) {
        if (actionProcess.running)
            return
        if (action === "restart") {
            actionProcess.command = ["systemctl", "--user", "restart", "duskr.service"]
        } else if (action === "record" || action === "toggle"
                   || action === "start" || action === "stop") {
            actionProcess.command = ["duskr", "toggle"]
        } else if (action === "cancel") {
            actionProcess.command = ["duskr", "cancel"]
        } else {
            return
        }
        actionProcess.running = true
    }

    function applyLine(line) {
        if (!line || line.length === 0)
            return
        let data
        try {
            data = JSON.parse(line)
        } catch (error) {
            return
        }
        available = true

        // `watch` interleaves partial lines - {"level":...} while recording,
        // {"transcript":...} on completion - with full status lines. Each field
        // is merged only when present; overwriting on every line would drop the
        // state back to idle between level updates and make the island morph
        // restart several times a second.
        if (data.class !== undefined)
            state = data.class
        if (data.tooltip !== undefined)
            tooltip = data.tooltip
        if (data.level !== undefined)
            level = Math.max(0, Math.min(1, Number(data.level)))
        if (data.ready !== undefined)
            ready = !!data.ready
        if (data.backend !== undefined)
            backend = data.backend
        if (data.model !== undefined)
            model = data.model === null ? "" : data.model

        levelActive = state === "recording" || state === "paused"
    }

    Process {
        id: watcher
        running: true
        command: ["duskr", "quickshell", "watch"]
        stdout: SplitParser { onRead: line => service.applyLine(line) }
        onExited: {
            service.available = false
            service.ready = false
            service.state = "stopped"
            service.tooltip = "Duskr is unavailable"
            service.level = 0
            service.levelActive = false
            retryTimer.start()
        }
    }

    Timer {
        id: retryTimer
        interval: 2000
        repeat: false
        onTriggered: watcher.running = true
    }

    Process { id: actionProcess }

    Component.onCompleted: controller.hyprwhsprService = service
}
