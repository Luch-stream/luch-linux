// Luch for the Plasma panel: the icon sits in the system tray; a click opens a popup with the receivers on the
// network and what to show on each. It talks to luchd through luchctl (from the Luch Flatpak).
import QtQuick
import QtQuick.Layouts
import org.kde.plasma.plasmoid
import org.kde.plasma.components as PC3
import org.kde.plasma.extras as PlasmaExtras
import org.kde.plasma.plasma5support as P5Support
import org.kde.kirigami as Kirigami

PlasmoidItem {
    id: root

    readonly property bool ru: Qt.locale().name.indexOf("ru") === 0
    function tr(en, ruText) { return ru ? ruText : en }
    readonly property string ctl: "flatpak run --command=luchctl toxblh.luch.Luch "

    property var receivers: []
    property string error: ""
    readonly property var mirroringNow: receivers.filter(r => r.mirroring)

    toolTipMainText: tr("Luch", "Луч")
    toolTipSubText: mirroringNow.length > 0
        ? tr("Showing on ", "Идёт показ: ") + mirroringNow[0].name
        : tr("Show the screen on a TV or phone", "Показать экран на ТВ или телефоне")

    P5Support.DataSource {
        id: exec
        engine: "executable"
        connectedSources: []
        property var handlers: ({})
        onNewData: (source, data) => {
            const cb = handlers[source]
            delete handlers[source]
            disconnectSource(source)
            if (cb) cb(data["exit code"], (data["stdout"] || "") + (data["stderr"] || ""))
        }
        function run(cmd, cb) { handlers[cmd] = cb; connectSource(cmd) }
    }

    function refresh() {
        exec.run(ctl + "list", (code, out) => {
            if (code !== 0) { root.error = out.trim(); root.receivers = []; return }
            root.error = ""
            const rows = []
            const lines = out.split("\n").slice(1)
            for (const l of lines) {
                if (l.length < 40 || l.indexOf("(no receivers)") === 0) continue
                const parts = l.trim().split(/\s+/)
                rows.push({ id: l.substr(0, 36), name: l.substr(39, 20).trim(), mirroring: parts[parts.length - 1] === "mirroring" })
            }
            root.receivers = rows
        })
    }
    // A show is already running somewhere else: ask before starting another (stop it, or show to both).
    property var pending: null
    function begin(id, what) {
        const others = receivers.filter(r => r.mirroring && r.id !== id)
        if (what === "stop" || others.length === 0) { act(id, what); return }
        pending = { id: id, what: what, others: others }
    }
    function confirmPending(stopOthers) {
        const p = pending
        pending = null
        if (!p) return
        if (stopOthers) p.others.forEach(o => act(o.id, "stop"))
        act(p.id, p.what)
    }
    function act(id, what) {
        exec.run(ctl + (what === "stop" ? "stop " : "mirror ") + id + (what === "stop" ? "" : " " + what), () => refresh())
    }

    Timer { interval: 4000; repeat: true; running: root.expanded; triggeredOnStart: true; onTriggered: root.refresh() }
    Timer { interval: 30000; repeat: true; running: !root.expanded; triggeredOnStart: true; onTriggered: root.refresh() }

    compactRepresentation: MouseArea {
        onClicked: root.expanded = !root.expanded
        hoverEnabled: true
        Kirigami.Icon {
            anchors.centerIn: parent
            width: Kirigami.Units.iconSizes.smallMedium     // the size of the other panel icons
            height: width
            source: Qt.resolvedUrl("luch-outline.svg")
            isMask: true
            color: root.mirroringNow.length > 0 ? "#ffb547" : Kirigami.Theme.textColor
            active: parent.containsMouse
        }
    }

    function kindIcon(name) {
        const n = name.toLowerCase()
        if (/pixel|phone|телефон|android|galaxy|iphone/.test(n)) return "smartphone"
        if (/tv|streamer|projector|проектор|box|mi /.test(n)) return "video-television"
        return "video-display"
    }

    fullRepresentation: PlasmaExtras.Representation {
        implicitWidth: Kirigami.Units.gridUnit * 24
        implicitHeight: Math.max(Kirigami.Units.gridUnit * 13, Math.min(Kirigami.Units.gridUnit * 30, header.height + list.contentHeight + Kirigami.Units.gridUnit * 2))
        collapseMarginsHint: true

        header: PlasmaExtras.PlasmoidHeading {
            id: header
            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing
                Kirigami.Icon { source: Qt.resolvedUrl("luch.svg"); implicitWidth: Kirigami.Units.iconSizes.medium; implicitHeight: implicitWidth }
                ColumnLayout {
                    spacing: 0
                    Kirigami.Heading { level: 3; text: root.tr("Luch", "Луч"); Layout.fillWidth: true }
                    PC3.Label {
                        text: root.mirroringNow.length > 0 ? root.tr("Showing on ", "Идёт показ: ") + root.mirroringNow[0].name
                                                            : root.tr("Where to show the screen", "Куда показывать экран")
                        opacity: 0.7
                        Layout.fillWidth: true
                        elide: Text.ElideRight
                    }
                }
            }
        }

        Rectangle {   // the question: another show is running
            anchors.fill: parent
            visible: root.pending !== null
            color: Qt.alpha(Kirigami.Theme.backgroundColor, 0.94)
            z: 10
            MouseArea { anchors.fill: parent }   // swallows clicks under the question
            ColumnLayout {
                anchors.centerIn: parent
                width: parent.width - Kirigami.Units.gridUnit * 3
                spacing: Kirigami.Units.largeSpacing
                Kirigami.Icon { source: "dialog-warning"; Layout.alignment: Qt.AlignHCenter; implicitWidth: Kirigami.Units.iconSizes.huge; implicitHeight: implicitWidth }
                Kirigami.Heading { level: 3; text: root.tr("A show is already running", "Показ уже идёт"); Layout.fillWidth: true; horizontalAlignment: Text.AlignHCenter; wrapMode: Text.Wrap }
                PC3.Label {
                    Layout.fillWidth: true; horizontalAlignment: Text.AlignHCenter; wrapMode: Text.Wrap
                    text: root.pending ? root.tr("Showing to ", "Сейчас идёт показ на: ") + root.pending.others.map(o => o.name).join(", ")
                                           + root.tr(". Stop it first, or show to both?", ". Остановить его перед новым или показывать на оба?") : ""
                }
                PC3.Button { Layout.fillWidth: true; highlighted: true; text: root.tr("Stop it and start", "Остановить и начать"); icon.name: "media-playback-stop"; onClicked: root.confirmPending(true) }
                PC3.Button { Layout.fillWidth: true; text: root.tr("Show to both", "Показывать на оба"); onClicked: root.confirmPending(false) }
                PC3.Button { Layout.fillWidth: true; flat: true; text: root.tr("Cancel", "Отмена"); onClicked: root.pending = null }
            }
        }

        contentItem: ListView {
            id: list
            model: root.receivers
            spacing: Kirigami.Units.largeSpacing
            topMargin: Kirigami.Units.largeSpacing
            bottomMargin: Kirigami.Units.largeSpacing
            leftMargin: Kirigami.Units.largeSpacing
            rightMargin: Kirigami.Units.largeSpacing
            clip: true
            delegate: Rectangle {
                width: list.width - list.leftMargin - list.rightMargin
                implicitHeight: card.implicitHeight + Kirigami.Units.largeSpacing * 2
                height: implicitHeight
                radius: Kirigami.Units.largeSpacing
                color: Qt.alpha(Kirigami.Theme.textColor, 0.06)
                border.width: modelData.mirroring ? 2 : 1
                border.color: modelData.mirroring ? "#ffb547" : Qt.alpha(Kirigami.Theme.textColor, 0.12)

                ColumnLayout {
                    id: card
                    anchors { left: parent.left; right: parent.right; top: parent.top; margins: Kirigami.Units.largeSpacing }
                    spacing: Kirigami.Units.largeSpacing

                    RowLayout {
                        spacing: Kirigami.Units.largeSpacing
                        Kirigami.Icon { source: root.kindIcon(modelData.name); implicitWidth: Kirigami.Units.iconSizes.large; implicitHeight: implicitWidth }
                        ColumnLayout {
                            spacing: 0
                            Layout.fillWidth: true
                            Kirigami.Heading { level: 4; text: modelData.name; Layout.fillWidth: true; elide: Text.ElideRight }
                            PC3.Label {
                                text: modelData.mirroring ? root.tr("showing now", "идёт показ") : root.tr("ready", "готов")
                                color: modelData.mirroring ? "#ffb547" : Kirigami.Theme.positiveTextColor
                                font.bold: modelData.mirroring
                            }
                        }
                        PC3.Button {
                            visible: !modelData.mirroring
                            highlighted: true
                            text: root.tr("Show screen", "Показать экран")
                            icon.name: "media-playback-start"
                            onClicked: root.begin(modelData.id, "fast")
                        }
                        PC3.Button {
                            visible: modelData.mirroring
                            text: root.tr("Stop", "Остановить")
                            icon.name: "media-playback-stop"
                            onClicked: root.act(modelData.id, "stop")
                        }
                    }

                    Flow {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing
                        PC3.Button { flat: true; text: root.tr("Pick a screen", "Выбрать экран"); icon.name: "monitor"; onClicked: root.begin(modelData.id, "monitor") }
                        PC3.Button { flat: true; text: root.tr("One window", "Одно окно"); icon.name: "window"; onClicked: root.begin(modelData.id, "window") }
                        PC3.Button { flat: true; text: root.tr("Second screen", "Второй экран"); icon.name: "video-display-add"; onClicked: root.begin(modelData.id, "virtual") }
                    }
                }
            }

            PlasmaExtras.PlaceholderMessage {
                anchors.centerIn: parent
                width: parent.width - Kirigami.Units.gridUnit * 2
                visible: list.count === 0
                iconName: root.error !== "" ? "dialog-error" : "network-wireless"
                text: root.error !== "" ? root.tr("Luch is not running", "Луч не запущен")
                                         : root.tr("No receivers found", "Приёмники не найдены")
                explanation: root.error !== "" ? root.error
                    : root.tr("Open the Luch app on the TV, projector or phone in the same network.",
                              "Откройте приложение Луч на ТВ, проекторе или телефоне в той же сети.")
            }
        }
    }
}
