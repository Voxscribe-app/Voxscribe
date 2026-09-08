import QtQuick
import Quickshell
import Quickshell.Io

// Native Duskr service for Quickshell.
//
// `duskr quickshell watch` holds one socket open and prints a JSON line on
// every state or level change, so there is no polling and no per-tick process.
// The exposed properties match the hyprwhspr service this replaces -
// available, state, tooltip, level, levelActive - so existing widgets keep
// working unchanged.
Item {
    id: service
    visible: false

    // Optional: assign a ShellController to have the service register itself.
    property var controller: null
    property string registerAs: ""

    property bool available: false
    property string state: "stopped"
    property string tooltip: "Duskr is unavailable"
    property real level: 0
    property bool levelActive: false
    property bool ready: false
    property string backend: ""
    property string model: ""
    property string mode: ""
    property string lastTranscript: ""

    signal transcript(string text)

    // "record" and "restart" are accepted so widgets written against the
    // hyprwhspr service need no edits, and "start"/"stop" still resolve now
    // that the CLI has only the one toggle.
    function perform(action) {
        const map = {
            "record": "toggle",
            "toggle": "toggle",
            "start": "toggle",
            "stop": "toggle",
            "cancel": "cancel",
            "submit": "submit",
            "restart": "restart"
        }
        const command = map[action]
        if (!command || actionProcess.running)
            return
        actionProcess.command = command === "restart"
            ? ["systemctl", "--user", "restart", "duskr.service"]
            : ["duskr", command]
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
        if (data.mode !== undefined)
            mode = data.mode
        if (data.transcript !== undefined && data.transcript !== null && data.transcript.length > 0) {
            lastTranscript = data.transcript
            service.transcript(data.transcript)
        }

        // The morph overlay keys off levelActive, so it has to follow the
        // recording state rather than whether the level happens to be non-zero.
        levelActive = state === "recording" || state === "paused"
    }

    function markUnavailable() {
        available = false
        ready = false
        state = "stopped"
        tooltip = "Duskr is unavailable"
        level = 0
        levelActive = false
    }

    Process {
        id: watcher
        running: true
        command: ["duskr", "quickshell", "watch"]
        stdout: SplitParser { onRead: line => service.applyLine(line) }
        // The daemon may not be up yet, or may be restarting; retry rather than
        // leaving the widget dead until the shell is reloaded.
        onExited: {
            service.markUnavailable()
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

    Component.onCompleted: {
        if (controller && registerAs.length > 0)
            controller[registerAs] = service
    }
}
