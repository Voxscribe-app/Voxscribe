import QtQuick
import Quickshell
import Quickshell.Io
import ".."

// Drop-in replacement for the hyprwhspr service, installed by `voxscribe migrate
// hyprwhspr`. The original is backed up first.
Item {
    id: service
    visible: false
    required property var controller

    property bool available: false
    property string state: "stopped"
    property string tooltip: "Voxscribe is unavailable"
    property real level: 0
    property bool levelActive: false

    property bool ready: false
    property string backend: ""
    property string model: ""

    function perform(action) {
        if (actionProcess.running)
            return
        if (action === "restart") {
            actionProcess.command = ["systemctl", "--user", "restart", "voxscribe.service"]
        } else if (action === "record" || action === "toggle"
                   || action === "start" || action === "stop") {
            actionProcess.command = ["voxscribe", "toggle"]
        } else if (action === "cancel") {
            actionProcess.command = ["voxscribe", "cancel"]
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
        command: ["voxscribe", "quickshell", "watch"]
        stdout: SplitParser { onRead: line => service.applyLine(line) }
        onExited: {
            service.available = false
            service.ready = false
            service.state = "stopped"
            service.tooltip = "Voxscribe is unavailable"
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
