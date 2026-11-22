const manifest = {"name":"Luch Streaming"};
const API_VERSION = 2;
const internalAPIConnection = window.__DECKY_SECRET_INTERNALS_DO_NOT_USE_OR_YOU_WILL_BE_FIRED_deckyLoaderAPIInit;
if (!internalAPIConnection) {
    throw new Error('[@decky/api]: Failed to connect to the loader as as the loader API was not initialized. This is likely a bug in Decky Loader.');
}
let api;
try {
    api = internalAPIConnection.connect(API_VERSION, manifest.name);
}
catch {
    api = internalAPIConnection.connect(1, manifest.name);
    console.warn(`[@decky/api] Requested API version ${API_VERSION} but the running loader only supports version 1. Some features may not work.`);
}
if (api._version != API_VERSION) {
    console.warn(`[@decky/api] Requested API version ${API_VERSION} but the running loader only supports version ${api._version}. Some features may not work.`);
}
const callable = api.callable;
const definePlugin = (fn) => {
    return (...args) => {
        return fn(...args);
    };
};

const en = {
    getReceiver: "Install the receiver on the other device",
    getReceiverText: "To show the screen there, that device needs the Luch receiver. Point a phone camera at the code to open the page with the app for Android.",
    appName: "Luch",
    tagline: "Your screen on a TV, projector or phone, with game-streaming latency",
    receivers: "Devices",
    none: "No devices found on this network.",
    notRunning: "Luch did not start on its own.",
    mirroring: "Showing: press to stop",
    busyTitle: "A show is already running",
    busyText: "Showing to %s. Stop it first, or show to both?",
    stopAndStart: "Stop it and start",
    both: "Show to both",
    cancel: "Cancel",
    mode: "Mode",
    size: "Size",
    codec: "Video codec",
    codecAuto: "Auto",
    sound: "Sound",
    alsoHere: "Also play on this device",
    alsoHereOff: "Off: the Deck stays silent while showing",
    screen: "Screen",
    dim: "Dim the Deck screen while showing",
    dimOff: "Brightness drops to the minimum 4 s after the menus close. Press Steam or ··· to bring it back; it also returns when the show stops",
    installTitle: "Luch is not installed yet",
    installText: "This panel needs Luch itself. It is downloaded and set up right here, no desktop mode needed.",
    installBtn: "Install Luch",
    installing: "Installing…",
    installFailed: "Could not install: ",
    errDownload: "could not download (no connection, or neither ALS nor GitHub answers)",
    stage1: "Downloading",
    stage2: "Installing",
    stage3: "Starting the service",
    stage4: "Checking that it works",
    starting: "Starting Luch…",
    startBtn: "Start Luch",
    fast: "Fast mirror",
    fast169: "Mirror 16:9",
    monitor: "Pick a screen",
    window: "One window",
    virtual: "Second screen",
};
const ru = {
    getReceiver: "Нужен приёмник на устройстве",
    getReceiverText: "Чтобы показать экран на устройстве, на нём должен стоять приёмник Луча. Наведите камеру телефона на код: откроется страница с приложением для Android.",
    appName: "Луч",
    tagline: "Экран на ТВ, проектор или телефон с задержкой игрового стриминга",
    receivers: "Устройства",
    none: "В сети не найдено устройств.",
    notRunning: "Луч не запустился сам.",
    mirroring: "Идёт показ: нажмите, чтобы остановить",
    busyTitle: "Показ уже идёт",
    busyText: "Сейчас идёт показ на: %s. Остановить его перед новым или показывать на оба?",
    stopAndStart: "Остановить и начать",
    both: "Показывать на оба",
    cancel: "Отмена",
    mode: "Режим",
    size: "Размер",
    codec: "Видеокодек",
    codecAuto: "Авто",
    sound: "Звук",
    alsoHere: "Играть звук и на этом устройстве",
    alsoHereOff: "Выключено: во время показа Deck молчит",
    screen: "Экран",
    dim: "Затемнять экран Deck при показе",
    dimOff: "Яркость падает до минимума через 4 с после закрытия меню. Нажмите Steam или «···», чтобы вернуть; при остановке показа она возвращается сама",
    installTitle: "Луч ещё не установлен",
    installText: "Панели нужен сам Луч. Он скачается и настроится прямо здесь, рабочий стол не нужен.",
    installBtn: "Установить Луч",
    installing: "Установка…",
    installFailed: "Не удалось установить: ",
    errDownload: "не удалось скачать (нет связи, или не отвечают ни ALS, ни GitHub)",
    stage1: "Скачивание",
    stage2: "Установка",
    stage3: "Запуск службы",
    stage4: "Проверка работоспособности",
    starting: "Запуск Луча…",
    startBtn: "Запустить Луч",
    fast: "Быстрое зеркало",
    fast169: "Зеркало 16:9",
    monitor: "Выбор экрана",
    window: "Одно окно",
    virtual: "Второй экран",
};
const it = {
    getReceiver: "Installa il ricevitore sull'altro dispositivo",
    getReceiverText: "Per mostrare lo schermo lì, quel dispositivo ha bisogno del ricevitore Luch. Inquadra il codice con la fotocamera del telefono per aprire la pagina con l'app per Android.",
    appName: "Luch",
    tagline: "Il tuo schermo su TV, proiettore o telefono, con la latenza del game streaming",
    receivers: "Dispositivi",
    none: "Nessun dispositivo trovato su questa rete.",
    notRunning: "Luch non si è avviato da solo.",
    mirroring: "Trasmissione in corso: premi per fermare",
    busyTitle: "Una trasmissione è già in corso",
    busyText: "Trasmissione su %s. Fermarla prima o trasmettere su entrambi?",
    stopAndStart: "Ferma e avvia",
    both: "Trasmetti su entrambi",
    cancel: "Annulla",
    mode: "Modalità",
    size: "Dimensione",
    codec: "Codec video",
    codecAuto: "Auto",
    sound: "Audio",
    alsoHere: "Riproduci l'audio anche su questo dispositivo",
    alsoHereOff: "Spento: il Deck resta muto durante la trasmissione",
    screen: "Schermo",
    dim: "Oscura lo schermo del Deck durante la trasmissione",
    dimOff: "La luminosità scende al minimo 4 s dopo la chiusura dei menu. Premi Steam o ··· per riportarla; torna anche quando la trasmissione finisce",
    installTitle: "Luch non è ancora installato",
    installText: "Questo pannello ha bisogno di Luch stesso. Viene scaricato e configurato qui, senza modalità desktop.",
    installBtn: "Installa Luch",
    installing: "Installazione…",
    installFailed: "Installazione non riuscita: ",
    errDownload: "download non riuscito (nessuna connessione, o né ALS né GitHub rispondono)",
    stage1: "Download",
    stage2: "Installazione",
    stage3: "Avvio del servizio",
    stage4: "Verifica del funzionamento",
    starting: "Avvio di Luch…",
    startBtn: "Avvia Luch",
    fast: "Specchio rapido",
    fast169: "Specchio 16:9",
    monitor: "Scegli uno schermo",
    window: "Una finestra",
    virtual: "Secondo schermo",
};
const TABLES = { en, ru, it };
// Steam's own name for the interface language (LocalizationManager) -> the table above.
const STEAM_LANG = { russian: "ru", italian: "it", english: "en" };
const lang = (() => {
    const raw = String(window.LocalizationManager?.m_rgLocalesToUse?.[0] ?? navigator.language ?? "en").toLowerCase();
    return STEAM_LANG[raw] ?? (TABLES[raw.slice(0, 2)] ? raw.slice(0, 2) : "en");
})();
const t = (key) => TABLES[lang]?.[key] ?? en[key] ?? key;

// QR codes of the receiver page on the site (generated once, see the commit message): rows of 33 modules, '1' dark.
const QR = {
    "ru": {
        "url": "https://plafonlinux.altlinux.team/luch-site/android.html",
        "rows": [
            "111111100100110101110011001111111",
            "100000101111101101011101101000001",
            "101110100110011100101001001011101",
            "101110100000011010110010001011101",
            "101110101011101011010111001011101",
            "100000100111111111110011101000001",
            "111111101010101010101010101111111",
            "000000000110010101001100100000000",
            "101010100111110011011101100010010",
            "101111000101010001101000111100011",
            "001100101000101010001100110100101",
            "010010011110011101001100100010000",
            "001001101111001111011100111001010",
            "000110010101101111000000001100101",
            "100001100110010110001000010011011",
            "101110011001011011011101101110010",
            "100001110111010101001100111000000",
            "000110011110011010000100101100011",
            "110110110000110000001000001000111",
            "110100001111101111110101001110000",
            "001001100011101001011100111001001",
            "001010001111110100001010111000111",
            "101100101001001000000010101100111",
            "010011000111111101010111110100010",
            "100110110100100001010100111111000",
            "000000001110101010001101100011011",
            "111111100001110010000111101010111",
            "100000100111010011011100100010000",
            "101110101101011110101100111111011",
            "101110100111111110001001000010001",
            "101110101101001110100101000011101",
            "100000100011011011101100111010010",
            "111111101110001111010101111010011"
        ]
    },
    "en": {
        "url": "https://plafonlinux.altlinux.team/luch-site/en/android.html",
        "rows": [
            "111111101100001011111101001111111",
            "100000101111101001000011101000001",
            "101110100110111100110001101011101",
            "101110101001100001000011101011101",
            "101110100000100010010100101011101",
            "100000100100011110111100101000001",
            "111111101010101010101010101111111",
            "000000001000001111010000100000000",
            "101101110001001111100101001001011",
            "110000000111010000011001001101101",
            "000010110010110001101111010111001",
            "000001000101100010001011100101000",
            "011111101110101001110000110111011",
            "010000010000011001010100010000110",
            "000100100101111010110000101011100",
            "010000001111100110110100011111100",
            "100011110000100110101111011011100",
            "101011000110101101000111101011011",
            "000100101001111111000010000110110",
            "111010010111000011001011010010011",
            "000100101001101011101100000001110",
            "100010001001001011111001001001001",
            "000010110001101011101001001111011",
            "011100010001000000011000110011001",
            "100000101000111011001010111111001",
            "000000001101111110010001100011000",
            "111111101000100010011011101010000",
            "100000101101110010101111100011110",
            "101110100011001100101111111110111",
            "101110101000110101101110000101001",
            "101110101011011100100011001101100",
            "100000100101000011110000100110001",
            "111111101001001011101101000010100"
        ]
    },
    "it": {
        "url": "https://plafonlinux.altlinux.team/luch-site/it/android.html",
        "rows": [
            "111111101011010000100010001111111",
            "100000101110011000110000001000001",
            "101110101100011000010101001011101",
            "101110100111001111001101101011101",
            "101110101101000110110010001011101",
            "100000100100101111001101001000001",
            "111111101010101010101010101111111",
            "000000000000111110100001000000000",
            "100111111101101011000001110010111",
            "111110001001011110010011000011100",
            "100110110000010101001111110011101",
            "100011010100010011111010010100110",
            "110010110011010010101011101100000",
            "111101010000001000100101100001000",
            "100100111000011110010100001111000",
            "010110011001001000111010010001101",
            "000111100101000010001011111111000",
            "111000010111001100110110011010101",
            "101110110110111100011001011101101",
            "001011000010110010111010100011101",
            "110011101011000111001000100101010",
            "111101000110100101110111000111000",
            "100010100100101111001101101011111",
            "100101011001010001101001000010111",
            "110011110011100000010001111110010",
            "000000001000001111100000100010110",
            "111111101100001110111111101010100",
            "100000101101100100100001100011111",
            "101110101111110000001011111110011",
            "101110101010000100011111110100111",
            "101110100000100111111000010110111",
            "100000100101110010000001010111111",
            "111111101101101111001001100110000"
        ]
    }
};
// The page for the language of the Deck: the site has Russian, English and Italian pages, English for the rest.
const qrFor = (lang) => QR[lang] ?? QR.en;
/** One SVG path for the dark modules, a run of modules in a row at a time. */
const qrPath = (rows) => {
    let d = "";
    rows.forEach((row, y) => {
        let x = 0;
        while (x < row.length) {
            if (row[x] === "1") {
                let e = x;
                while (e < row.length && row[e] === "1")
                    e++;
                d += `M${x} ${y}h${e - x}v1h-${e - x}z`;
                x = e;
            }
            else
                x++;
        }
    });
    return d;
};

// The Luch mark as an outline, the same as the one in the Plasma panel widget (luch-outline.svg), in the colour of the text.
const LuchIcon = () => (window.SP_REACT.createElement("svg", { width: "1em", height: "1em", viewBox: "0 0 22 22", fill: "none", stroke: "currentColor", strokeWidth: "1.4", strokeLinecap: "round", strokeLinejoin: "round" },
    window.SP_REACT.createElement("rect", { x: "3.5", y: "2.5", width: "15", height: "9", rx: "1.5" }),
    window.SP_REACT.createElement("path", { d: "M7.5 14.5 L5.5 12.5 M14.5 14.5 L16.5 12.5" }),
    window.SP_REACT.createElement("rect", { x: "7.5", y: "14.5", width: "7", height: "4", rx: "1" }),
    window.SP_REACT.createElement("path", { d: "M6 20.5 H16" })));
const state = callable("state");
const choose = callable("choose");
const start = callable("start");
const stop = callable("stop");
const chooseCodec = callable("choose_codec");
const setLocalAudio = callable("set_local_audio");
const setDimScreen = callable("set_dim_screen");
const installLuch = callable("install_luch");
const startService = callable("start_service");
const srcTitle = (v) => t(v);
// A row of text with air around it (the panel packs its rows tightly).
const Gap = ({ children, below = 10, size, dim }) => (window.SP_REACT.createElement(DFL.PanelSectionRow, null,
    window.SP_REACT.createElement("div", { style: { paddingBottom: below, fontSize: size, opacity: dim ? 0.7 : 1 } }, children)));
// 1920x1080 -> 1080p
const short = (size) => (/^\d+x(\d+)$/.exec(size) ? `${/^\d+x(\d+)$/.exec(size)[1]}p` : size);
function Content() {
    const [s, setS] = SP_REACT.useState({ up: false, receivers: [] });
    // luchd takes a few seconds after a start or an install: a loading bar first, the hint only when it does not come up
    const [downFor, setDownFor] = SP_REACT.useState(0);
    // the mode, codec and size of a receiver are folded away until its arrow is pressed
    const [open, setOpen] = SP_REACT.useState({});
    const [qrOpen, setQrOpen] = SP_REACT.useState(false);
    // a failed call counts as "down" too, so that the start timer (and the Start button after it) still runs
    const refresh = async () => {
        try {
            setS(await state());
        }
        catch {
            setS({ up: false, receivers: [] });
        }
    };
    SP_REACT.useEffect(() => {
        refresh();
        const t = setInterval(refresh, 2000);
        return () => clearInterval(t);
    }, []);
    SP_REACT.useEffect(() => {
        setDownFor((d) => (s.up ? 0 : d + 2));
    }, [s]);
    if (!s.up && s.installed === false) {
        const i = s.install;
        const pct = i?.stage === 1 && parseInt(i.text) ? parseInt(i.text) / 4 : 0;
        return (window.SP_REACT.createElement(DFL.PanelSection, { title: t("installTitle") },
            window.SP_REACT.createElement(Gap, { size: 12, dim: true }, t("tagline")),
            window.SP_REACT.createElement(Gap, { below: 16 }, t("installText")),
            i?.running && (window.SP_REACT.createElement(window.SP_REACT.Fragment, null,
                window.SP_REACT.createElement(Gap, { below: 8 }, `${i.stage}/4 · ${t("stage" + i.stage)} ${i.text}`),
                window.SP_REACT.createElement(Gap, { below: 16 },
                    window.SP_REACT.createElement(DFL.ProgressBar, { nProgress: i.stage > 1 ? (i.stage - 1) * 25 + pct : pct })))),
            i?.error && window.SP_REACT.createElement(Gap, { below: 16 }, t("installFailed") + (i.error === "download" ? t("errDownload") : i.error)),
            window.SP_REACT.createElement(DFL.PanelSectionRow, null,
                window.SP_REACT.createElement(DFL.ButtonItem, { layout: "below", disabled: !!i?.running, onClick: () => installLuch().then(refresh) }, i?.running ? t("installing") : t("installBtn")))));
    }
    if (!s.up) {
        return (window.SP_REACT.createElement(DFL.PanelSection, { title: t("appName") }, downFor < 30 ? (window.SP_REACT.createElement(window.SP_REACT.Fragment, null,
            window.SP_REACT.createElement(Gap, { below: 10 }, t("starting")),
            window.SP_REACT.createElement(Gap, { below: 10 },
                window.SP_REACT.createElement(DFL.ProgressBar, { indeterminate: true, nProgress: 0 })))) : (window.SP_REACT.createElement(window.SP_REACT.Fragment, null,
            window.SP_REACT.createElement(Gap, { below: 12 }, t("notRunning")),
            window.SP_REACT.createElement(DFL.PanelSectionRow, null,
                window.SP_REACT.createElement(DFL.ButtonItem, { layout: "below", onClick: () => startService().then(refresh) }, t("startBtn")))))));
    }
    return (window.SP_REACT.createElement(window.SP_REACT.Fragment, null,
        window.SP_REACT.createElement(DFL.PanelSection, { title: t("receivers") },
            s.receivers.length === 0 && window.SP_REACT.createElement(Gap, null, t("none")),
            s.receivers.map((r) => (window.SP_REACT.createElement("div", { key: r.id, style: { paddingBottom: 14 } },
                window.SP_REACT.createElement(DFL.PanelSectionRow, null,
                    window.SP_REACT.createElement(DFL.Focusable, { style: { display: "flex", gap: "8px", alignItems: "stretch" }, "flow-children": "horizontal" },
                        window.SP_REACT.createElement(DFL.DialogButton, { style: { flex: 1, minWidth: 0, textAlign: "left", padding: "10px 14px" }, onClick: async () => {
                                if (r.mirroring) {
                                    await stop(r.id);
                                    refresh();
                                    return;
                                }
                                // Another show is running: ask what to do with it first.
                                const others = s.receivers.filter((x) => x.mirroring && x.id !== r.id);
                                if (others.length === 0) {
                                    await start(r.id, r.source, r.size);
                                    refresh();
                                    return;
                                }
                                DFL.showModal(window.SP_REACT.createElement(DFL.ConfirmModal, { strTitle: t("busyTitle"), strDescription: t("busyText").replace("%s", others.map((x) => x.name).join(", ")), strOKButtonText: t("stopAndStart"), strMiddleButtonText: t("both"), strCancelButtonText: t("cancel"), onOK: async () => {
                                        for (const o of others)
                                            await stop(o.id);
                                        await start(r.id, r.source, r.size);
                                        refresh();
                                    }, onMiddleButton: async () => {
                                        await start(r.id, r.source, r.size);
                                        refresh();
                                    } }));
                            } },
                            window.SP_REACT.createElement("div", { style: { fontWeight: 600 } },
                                r.mirroring ? "■ " : "▶ ",
                                r.name),
                            window.SP_REACT.createElement("div", { style: { fontSize: "12px", opacity: 0.7, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", marginTop: 2 } }, r.mirroring ? t("mirroring") : `${srcTitle(r.source)} · ${short(r.size)}`)),
                        window.SP_REACT.createElement(DFL.DialogButton, { style: { width: "44px", minWidth: "44px", padding: "0", height: "auto" }, onClick: () => setOpen((o) => ({ ...o, [r.id]: !o[r.id] })) }, open[r.id] ? "▴" : "▾"))),
                open[r.id] && (window.SP_REACT.createElement(window.SP_REACT.Fragment, null,
                    window.SP_REACT.createElement(DFL.PanelSectionRow, null,
                        window.SP_REACT.createElement(DFL.DropdownItem, { label: t("mode"), rgOptions: (s.sources ?? []).map((v) => ({ data: v, label: srcTitle(v) })), selectedOption: r.source, onChange: async (o) => {
                                await choose(r.id, o.data, r.size);
                                refresh();
                            } })),
                    window.SP_REACT.createElement(DFL.PanelSectionRow, null,
                        window.SP_REACT.createElement(DFL.DropdownItem, { label: t("codec"), rgOptions: [
                                { data: "auto", label: t("codecAuto") },
                                { data: "hevc", label: "HEVC (H.265)" },
                                { data: "avc", label: "H.264" },
                            ], selectedOption: r.codec ?? "auto", onChange: async (o) => {
                                await chooseCodec(r.id, o.data);
                                refresh();
                            } })),
                    window.SP_REACT.createElement(DFL.PanelSectionRow, null,
                        window.SP_REACT.createElement(DFL.DropdownItem, { label: t("size"), rgOptions: (s.sizes ?? []).map((v) => ({ data: v, label: v.replace("x", "×") })), selectedOption: r.size, onChange: async (o) => {
                                await choose(r.id, r.source, o.data);
                                refresh();
                            } }))))))),
            window.SP_REACT.createElement(DFL.PanelSectionRow, null,
                window.SP_REACT.createElement(DFL.DialogButton, { style: { width: "100%", textAlign: "left", padding: "10px 14px", display: "flex", justifyContent: "space-between", alignItems: "center", gap: 8 }, onClick: () => setQrOpen((o) => !o) },
                    window.SP_REACT.createElement("span", { style: { fontSize: "13px" } }, t("getReceiver")),
                    window.SP_REACT.createElement("span", null, qrOpen ? "▴" : "▾"))),
            qrOpen && (window.SP_REACT.createElement(window.SP_REACT.Fragment, null,
                window.SP_REACT.createElement(Gap, { below: 10, size: 12, dim: true }, t("getReceiverText")),
                window.SP_REACT.createElement(DFL.PanelSectionRow, null,
                    window.SP_REACT.createElement("div", { style: { display: "flex", justifyContent: "center", paddingBottom: 6 } },
                        window.SP_REACT.createElement("svg", { width: "124", height: "124", viewBox: "-4 -4 41 41", shapeRendering: "crispEdges", style: { background: "#fff", borderRadius: 6 } },
                            window.SP_REACT.createElement("path", { d: qrPath(qrFor(lang).rows), fill: "#000" })))),
                window.SP_REACT.createElement(Gap, { below: 4, size: 11, dim: true }, qrFor(lang).url.replace("https://", ""))))),
        window.SP_REACT.createElement(DFL.PanelSection, { title: t("sound") },
            window.SP_REACT.createElement(DFL.PanelSectionRow, null,
                window.SP_REACT.createElement(DFL.ToggleField, { label: t("alsoHere"), description: t("alsoHereOff"), checked: s.local_audio ?? true, onChange: async (on) => {
                        await setLocalAudio(on);
                        refresh();
                    } }))),
        window.SP_REACT.createElement(DFL.PanelSection, { title: t("screen") },
            window.SP_REACT.createElement(DFL.PanelSectionRow, null,
                window.SP_REACT.createElement(DFL.ToggleField, { label: t("dim"), description: t("dimOff"), checked: s.dim_screen ?? false, onChange: async (on) => {
                        await setDimScreen(on);
                        refresh();
                    } })))));
}
var index = definePlugin(() => ({
    name: t("appName"),
    titleView: window.SP_REACT.createElement("div", { className: DFL.staticClasses.Title }, t("appName")),
    content: window.SP_REACT.createElement(Content, null),
    icon: window.SP_REACT.createElement(LuchIcon, null),
    onDismount() { },
}));

export { index as default };
//# sourceMappingURL=index.js.map
