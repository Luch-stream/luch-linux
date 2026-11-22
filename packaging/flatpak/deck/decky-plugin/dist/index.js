const manifest = {"name":"Luch"};
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
const uk = {
    appName: "Промінь",
    tagline: "Екран на ТБ, проєктор чи телефон із затримкою ігрового стримінгу",
    receivers: "Пристрої",
    none: "У мережі не знайдено пристроїв.",
    notRunning: "Промінь не запустився сам.",
    mirroring: "Триває показ: натисніть, щоб зупинити",
    busyTitle: "Показ уже триває",
    busyText: "Зараз показ на: %s. Зупинити його перед новим чи показувати на обидва?",
    stopAndStart: "Зупинити й почати",
    both: "Показувати на обидва",
    cancel: "Скасувати",
    mode: "Режим",
    size: "Розмір",
    codec: "Відеокодек",
    codecAuto: "Авто",
    sound: "Звук",
    alsoHere: "Грати звук і на цьому пристрої",
    alsoHereOff: "Вимкнено: під час показу Deck мовчить",
    screen: "Екран",
    dim: "Затемнювати екран Deck під час показу",
    dimOff: "Яскравість падає до мінімуму через 4 с після закриття меню. Натисніть Steam або «···», щоб повернути; вона повертається й після зупинки показу",
    installTitle: "Промінь ще не встановлено",
    installText: "Панелі потрібен сам Промінь. Він завантажиться й налаштується просто тут, робочий стіл не потрібен.",
    installBtn: "Встановити Промінь",
    installing: "Встановлення…",
    installFailed: "Не вдалося встановити: ",
    errDownload: "не вдалося завантажити (немає зв'язку або не відповідають ні ALS, ні GitHub)",
    stage1: "Завантаження",
    stage2: "Встановлення",
    stage3: "Запуск служби",
    stage4: "Перевірка роботи",
    starting: "Запуск Променя…",
    startBtn: "Запустити Промінь",
    fast: "Швидке дзеркало",
    fast169: "Дзеркало 16:9",
    monitor: "Вибір екрана",
    window: "Одне вікно",
    virtual: "Другий екран",
};
const de = {
    appName: "Luch",
    tagline: "Dein Bildschirm auf TV, Beamer oder Handy, mit der Latenz von Spiele-Streaming",
    receivers: "Geräte",
    none: "Keine Geräte im Netzwerk gefunden.",
    notRunning: "Luch ist nicht von selbst gestartet.",
    mirroring: "Übertragung läuft: zum Beenden drücken",
    busyTitle: "Eine Übertragung läuft bereits",
    busyText: "Übertragung auf %s läuft. Zuerst beenden oder auf beide übertragen?",
    stopAndStart: "Beenden und starten",
    both: "Auf beide übertragen",
    cancel: "Abbrechen",
    mode: "Modus",
    size: "Größe",
    codec: "Video-Codec",
    codecAuto: "Auto",
    sound: "Ton",
    alsoHere: "Ton auch auf diesem Gerät abspielen",
    alsoHereOff: "Aus: Das Deck bleibt während der Übertragung stumm",
    screen: "Bildschirm",
    dim: "Deck-Bildschirm während der Übertragung abdunkeln",
    dimOff: "Die Helligkeit sinkt 4 s nach dem Schließen der Menüs auf das Minimum. Steam oder ··· drücken, um sie zurückzuholen; sie kommt auch zurück, wenn die Übertragung endet",
    installTitle: "Luch ist noch nicht installiert",
    installText: "Dieses Panel braucht Luch selbst. Es wird hier heruntergeladen und eingerichtet, ohne Desktop-Modus.",
    installBtn: "Luch installieren",
    installing: "Installation…",
    installFailed: "Installation fehlgeschlagen: ",
    errDownload: "Download nicht möglich (keine Verbindung, oder weder ALS noch GitHub antwortet)",
    stage1: "Herunterladen",
    stage2: "Installieren",
    stage3: "Dienst starten",
    stage4: "Funktion prüfen",
    starting: "Luch startet…",
    startBtn: "Luch starten",
    fast: "Schneller Spiegel",
    fast169: "Spiegel 16:9",
    monitor: "Bildschirm wählen",
    window: "Ein Fenster",
    virtual: "Zweiter Bildschirm",
};
const fr = {
    appName: "Luch",
    tagline: "Votre écran sur une TV, un projecteur ou un téléphone, avec la latence du streaming de jeu",
    receivers: "Appareils",
    none: "Aucun appareil trouvé sur ce réseau.",
    notRunning: "Luch ne s'est pas lancé tout seul.",
    mirroring: "Diffusion en cours : appuyez pour arrêter",
    busyTitle: "Une diffusion est déjà en cours",
    busyText: "Diffusion vers %s. L'arrêter d'abord, ou diffuser vers les deux ?",
    stopAndStart: "Arrêter et démarrer",
    both: "Diffuser vers les deux",
    cancel: "Annuler",
    mode: "Mode",
    size: "Taille",
    codec: "Codec vidéo",
    codecAuto: "Auto",
    sound: "Son",
    alsoHere: "Jouer aussi le son sur cet appareil",
    alsoHereOff: "Désactivé : la Deck reste muette pendant la diffusion",
    screen: "Écran",
    dim: "Assombrir l'écran de la Deck pendant la diffusion",
    dimOff: "La luminosité tombe au minimum 4 s après la fermeture des menus. Appuyez sur Steam ou ··· pour la rétablir ; elle revient aussi à l'arrêt de la diffusion",
    installTitle: "Luch n'est pas encore installé",
    installText: "Ce panneau a besoin de Luch lui-même. Il est téléchargé et configuré ici, sans mode bureau.",
    installBtn: "Installer Luch",
    installing: "Installation…",
    installFailed: "Échec de l'installation : ",
    errDownload: "téléchargement impossible (pas de connexion, ou ni ALS ni GitHub ne répond)",
    stage1: "Téléchargement",
    stage2: "Installation",
    stage3: "Démarrage du service",
    stage4: "Vérification du fonctionnement",
    starting: "Démarrage de Luch…",
    startBtn: "Démarrer Luch",
    fast: "Miroir rapide",
    fast169: "Miroir 16:9",
    monitor: "Choisir un écran",
    window: "Une fenêtre",
    virtual: "Second écran",
};
const es = {
    appName: "Luch",
    tagline: "Tu pantalla en una TV, proyector o móvil, con la latencia del streaming de juegos",
    receivers: "Dispositivos",
    none: "No se encontraron dispositivos en esta red.",
    notRunning: "Luch no se inició solo.",
    mirroring: "Emitiendo: pulsa para detener",
    busyTitle: "Ya hay una emisión en curso",
    busyText: "Emitiendo a %s. ¿Detenerla primero o emitir a ambos?",
    stopAndStart: "Detener y empezar",
    both: "Emitir a ambos",
    cancel: "Cancelar",
    mode: "Modo",
    size: "Tamaño",
    codec: "Códec de vídeo",
    codecAuto: "Auto",
    sound: "Sonido",
    alsoHere: "Reproducir también el sonido en este dispositivo",
    alsoHereOff: "Desactivado: la Deck queda en silencio durante la emisión",
    screen: "Pantalla",
    dim: "Atenuar la pantalla de la Deck durante la emisión",
    dimOff: "El brillo baja al mínimo 4 s después de cerrar los menús. Pulsa Steam o ··· para recuperarlo; también vuelve al detener la emisión",
    installTitle: "Luch aún no está instalado",
    installText: "Este panel necesita Luch. Se descarga y configura aquí mismo, sin modo escritorio.",
    installBtn: "Instalar Luch",
    installing: "Instalando…",
    installFailed: "No se pudo instalar: ",
    errDownload: "no se pudo descargar (sin conexión, o ni ALS ni GitHub responden)",
    stage1: "Descargando",
    stage2: "Instalando",
    stage3: "Iniciando el servicio",
    stage4: "Comprobando que funciona",
    starting: "Iniciando Luch…",
    startBtn: "Iniciar Luch",
    fast: "Espejo rápido",
    fast169: "Espejo 16:9",
    monitor: "Elegir pantalla",
    window: "Una ventana",
    virtual: "Segunda pantalla",
};
const it = {
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
const pt = {
    appName: "Luch",
    tagline: "Sua tela na TV, projetor ou celular, com a latência do streaming de jogos",
    receivers: "Dispositivos",
    none: "Nenhum dispositivo encontrado nesta rede.",
    notRunning: "O Luch não iniciou sozinho.",
    mirroring: "Transmitindo: pressione para parar",
    busyTitle: "Já há uma transmissão em andamento",
    busyText: "Transmitindo para %s. Parar primeiro ou transmitir para os dois?",
    stopAndStart: "Parar e iniciar",
    both: "Transmitir para os dois",
    cancel: "Cancelar",
    mode: "Modo",
    size: "Tamanho",
    codec: "Codec de vídeo",
    codecAuto: "Auto",
    sound: "Som",
    alsoHere: "Tocar o som também neste dispositivo",
    alsoHereOff: "Desligado: o Deck fica em silêncio durante a transmissão",
    screen: "Tela",
    dim: "Escurecer a tela do Deck durante a transmissão",
    dimOff: "O brilho cai ao mínimo 4 s depois de fechar os menus. Pressione Steam ou ··· para voltar; ele também volta quando a transmissão para",
    installTitle: "O Luch ainda não está instalado",
    installText: "Este painel precisa do próprio Luch. Ele é baixado e configurado aqui mesmo, sem modo desktop.",
    installBtn: "Instalar o Luch",
    installing: "Instalando…",
    installFailed: "Não foi possível instalar: ",
    errDownload: "não foi possível baixar (sem conexão, ou nem ALS nem GitHub respondem)",
    stage1: "Baixando",
    stage2: "Instalando",
    stage3: "Iniciando o serviço",
    stage4: "Verificando o funcionamento",
    starting: "Iniciando o Luch…",
    startBtn: "Iniciar o Luch",
    fast: "Espelho rápido",
    fast169: "Espelho 16:9",
    monitor: "Escolher uma tela",
    window: "Uma janela",
    virtual: "Segunda tela",
};
const pl = {
    appName: "Luch",
    tagline: "Twój ekran na telewizorze, projektorze lub telefonie, z opóźnieniem streamingu gier",
    receivers: "Urządzenia",
    none: "Nie znaleziono urządzeń w tej sieci.",
    notRunning: "Luch nie uruchomił się sam.",
    mirroring: "Trwa transmisja: naciśnij, aby zatrzymać",
    busyTitle: "Transmisja już trwa",
    busyText: "Transmisja na %s. Zatrzymać ją najpierw, czy nadawać na oba?",
    stopAndStart: "Zatrzymaj i zacznij",
    both: "Nadawaj na oba",
    cancel: "Anuluj",
    mode: "Tryb",
    size: "Rozmiar",
    codec: "Kodek wideo",
    codecAuto: "Auto",
    sound: "Dźwięk",
    alsoHere: "Odtwarzaj dźwięk także na tym urządzeniu",
    alsoHereOff: "Wyłączone: Deck milczy podczas transmisji",
    screen: "Ekran",
    dim: "Przyciemnij ekran Decka podczas transmisji",
    dimOff: "Jasność spada do minimum 4 s po zamknięciu menu. Naciśnij Steam lub ···, aby ją przywrócić; wraca też po zatrzymaniu transmisji",
    installTitle: "Luch nie jest jeszcze zainstalowany",
    installText: "Ten panel potrzebuje samego Lucha. Zostanie pobrany i skonfigurowany tutaj, bez trybu pulpitu.",
    installBtn: "Zainstaluj Lucha",
    installing: "Instalowanie…",
    installFailed: "Nie udało się zainstalować: ",
    errDownload: "nie udało się pobrać (brak połączenia lub ani ALS, ani GitHub nie odpowiada)",
    stage1: "Pobieranie",
    stage2: "Instalowanie",
    stage3: "Uruchamianie usługi",
    stage4: "Sprawdzanie działania",
    starting: "Uruchamianie Lucha…",
    startBtn: "Uruchom Lucha",
    fast: "Szybkie lustro",
    fast169: "Lustro 16:9",
    monitor: "Wybór ekranu",
    window: "Jedno okno",
    virtual: "Drugi ekran",
};
const zh = {
    appName: "Luch",
    tagline: "以游戏串流级别的延迟，把屏幕投到电视、投影仪或手机",
    receivers: "设备",
    none: "在此网络中未找到设备。",
    notRunning: "Luch 没有自动启动。",
    mirroring: "正在投屏：按下以停止",
    busyTitle: "已有投屏正在进行",
    busyText: "正在投到 %s。先停止它，还是同时投到两个设备？",
    stopAndStart: "停止并开始",
    both: "同时投到两个",
    cancel: "取消",
    mode: "模式",
    size: "分辨率",
    codec: "视频编码",
    codecAuto: "自动",
    sound: "声音",
    alsoHere: "同时在本设备上播放声音",
    alsoHereOff: "关闭：投屏时 Deck 保持静音",
    screen: "屏幕",
    dim: "投屏时调暗 Deck 屏幕",
    dimOff: "菜单关闭 4 秒后亮度降到最低。按 Steam 或 ··· 恢复；投屏停止时也会自动恢复",
    installTitle: "尚未安装 Luch",
    installText: "此面板需要 Luch 本体。它会在这里直接下载并配置，无需桌面模式。",
    installBtn: "安装 Luch",
    installing: "正在安装…",
    installFailed: "安装失败：",
    errDownload: "下载失败（无网络，或 ALS 与 GitHub 均无响应）",
    stage1: "下载中",
    stage2: "安装中",
    stage3: "启动服务",
    stage4: "检查是否正常",
    starting: "正在启动 Luch…",
    startBtn: "启动 Luch",
    fast: "快速镜像",
    fast169: "镜像 16:9",
    monitor: "选择屏幕",
    window: "单个窗口",
    virtual: "第二屏幕",
};
const TABLES = { en, ru, uk, de, fr, es, it, pt, pl, zh };
// Steam's own name for the interface language (LocalizationManager) -> the table above.
const STEAM_LANG = {
    russian: "ru", ukrainian: "uk", german: "de", french: "fr", spanish: "es", latam: "es", italian: "it",
    brazilian: "pt", portuguese: "pt", polish: "pl", schinese: "zh", tchinese: "zh", english: "en",
};
const lang = (() => {
    const raw = String(window.LocalizationManager?.m_rgLocalesToUse?.[0] ?? navigator.language ?? "en").toLowerCase();
    return STEAM_LANG[raw] ?? (TABLES[raw.slice(0, 2)] ? raw.slice(0, 2) : "en");
})();
const t = (key) => TABLES[lang]?.[key] ?? en[key] ?? key;

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
    const refresh = async () => setS(await state());
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
                            } })))))))),
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
