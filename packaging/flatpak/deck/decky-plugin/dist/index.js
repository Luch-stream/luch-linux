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

var DefaultContext = {
  color: undefined,
  size: undefined,
  className: undefined,
  style: undefined,
  attr: undefined
};
var IconContext = SP_REACT.createContext && /*#__PURE__*/SP_REACT.createContext(DefaultContext);

var _excluded = ["attr", "size", "title"];
function _objectWithoutProperties(e, t) { if (null == e) return {}; var o, r, i = _objectWithoutPropertiesLoose(e, t); if (Object.getOwnPropertySymbols) { var n = Object.getOwnPropertySymbols(e); for (r = 0; r < n.length; r++) o = n[r], -1 === t.indexOf(o) && {}.propertyIsEnumerable.call(e, o) && (i[o] = e[o]); } return i; }
function _objectWithoutPropertiesLoose(r, e) { if (null == r) return {}; var t = {}; for (var n in r) if ({}.hasOwnProperty.call(r, n)) { if (-1 !== e.indexOf(n)) continue; t[n] = r[n]; } return t; }
function _extends() { return _extends = Object.assign ? Object.assign.bind() : function (n) { for (var e = 1; e < arguments.length; e++) { var t = arguments[e]; for (var r in t) ({}).hasOwnProperty.call(t, r) && (n[r] = t[r]); } return n; }, _extends.apply(null, arguments); }
function ownKeys(e, r) { var t = Object.keys(e); if (Object.getOwnPropertySymbols) { var o = Object.getOwnPropertySymbols(e); r && (o = o.filter(function (r) { return Object.getOwnPropertyDescriptor(e, r).enumerable; })), t.push.apply(t, o); } return t; }
function _objectSpread(e) { for (var r = 1; r < arguments.length; r++) { var t = null != arguments[r] ? arguments[r] : {}; r % 2 ? ownKeys(Object(t), true).forEach(function (r) { _defineProperty(e, r, t[r]); }) : Object.getOwnPropertyDescriptors ? Object.defineProperties(e, Object.getOwnPropertyDescriptors(t)) : ownKeys(Object(t)).forEach(function (r) { Object.defineProperty(e, r, Object.getOwnPropertyDescriptor(t, r)); }); } return e; }
function _defineProperty(e, r, t) { return (r = _toPropertyKey(r)) in e ? Object.defineProperty(e, r, { value: t, enumerable: true, configurable: true, writable: true }) : e[r] = t, e; }
function _toPropertyKey(t) { var i = _toPrimitive(t, "string"); return "symbol" == typeof i ? i : i + ""; }
function _toPrimitive(t, r) { if ("object" != typeof t || !t) return t; var e = t[Symbol.toPrimitive]; if (void 0 !== e) { var i = e.call(t, r); if ("object" != typeof i) return i; throw new TypeError("@@toPrimitive must return a primitive value."); } return ("string" === r ? String : Number)(t); }
function Tree2Element(tree) {
  return tree && tree.map((node, i) => /*#__PURE__*/SP_REACT.createElement(node.tag, _objectSpread({
    key: i
  }, node.attr), Tree2Element(node.child)));
}
function GenIcon(data) {
  return props => /*#__PURE__*/SP_REACT.createElement(IconBase, _extends({
    attr: _objectSpread({}, data.attr)
  }, props), Tree2Element(data.child));
}
function IconBase(props) {
  var elem = conf => {
    var attr = props.attr,
      size = props.size,
      title = props.title,
      svgProps = _objectWithoutProperties(props, _excluded);
    var computedSize = size || conf.size || "1em";
    var className;
    if (conf.className) className = conf.className;
    if (props.className) className = (className ? className + " " : "") + props.className;
    return /*#__PURE__*/SP_REACT.createElement("svg", _extends({
      stroke: "currentColor",
      fill: "currentColor",
      strokeWidth: "0"
    }, conf.attr, attr, svgProps, {
      className: className,
      style: _objectSpread(_objectSpread({
        color: props.color || conf.color
      }, conf.style), props.style),
      height: computedSize,
      width: computedSize,
      xmlns: "http://www.w3.org/2000/svg"
    }), title && /*#__PURE__*/SP_REACT.createElement("title", null, title), props.children);
  };
  return IconContext !== undefined ? /*#__PURE__*/SP_REACT.createElement(IconContext.Consumer, null, conf => elem(conf)) : elem(DefaultContext);
}

// THIS FILE IS AUTO GENERATED
function FaVideo (props) {
  return GenIcon({"attr":{"viewBox":"0 0 576 512"},"child":[{"tag":"path","attr":{"d":"M336.2 64H47.8C21.4 64 0 85.4 0 111.8v288.4C0 426.6 21.4 448 47.8 448h288.4c26.4 0 47.8-21.4 47.8-47.8V111.8c0-26.4-21.4-47.8-47.8-47.8zm189.4 37.7L416 177.3v157.4l109.6 75.5c21.2 14.6 50.4-.3 50.4-25.8V127.5c0-25.4-29.1-40.4-50.4-25.8z"},"child":[]}]})(props);
}

const state = callable("state");
const choose = callable("choose");
const start = callable("start");
const stop = callable("stop");
const setLocalAudio = callable("set_local_audio");
const setDimScreen = callable("set_dim_screen");
// Steam UI language: Russian if the Deck is set to Russian, English otherwise.
const isRu = (() => {
    const l = window.LocalizationManager?.m_rgLocalesToUse?.[0] ?? navigator.language ?? "";
    return /^(ru|russian)/i.test(String(l));
})();
const STR = {
    receivers: ["Receivers", "Приёмники"],
    none: ["No receivers found on this network.", "В сети не найдено приёмников."],
    notRunning: [
        "Luch is not running. Start the luchd service (flatpak run dev.luch.Luch).",
        "Луч не запущен. Запустите службу luchd (flatpak run dev.luch.Luch).",
    ],
    mirroring: ["Mirroring — press to stop", "Идёт показ — нажмите, чтобы остановить"],
    connected: ["Connected", "Подключён"],
    mode: ["Mode", "Режим"],
    size: ["Size", "Размер"],
    sound: ["Sound", "Звук"],
    alsoHere: ["Also play on this device", "Играть звук и на этом устройстве"],
    screen: ["Screen", "Экран"],
    dim: ["Turn off the Deck screen while mirroring", "Гасить экран Deck при показе"],
    dimOff: [
        "Goes dark 4 s after the menus close; lights up when you open a menu or stop the mirror",
        "Гаснет через 4 с после закрытия меню; включается при открытии меню и остановке показа",
    ],
    alsoHereOff: [
        "Off: the Deck stays silent while mirroring",
        "Выключено: во время показа Deck молчит",
    ],
};
const t = (k) => STR[k][isRu ? 1 : 0];
const SOURCE_TITLES = {
    fast: ["Fast mirror", "Быстрое зеркало"],
    fast169: ["Mirror 16:9", "Зеркало 16:9"],
    monitor: ["Pick a screen", "Выбор экрана"],
    window: ["One window", "Одно окно"],
    virtual: ["Second screen", "Второй экран"],
};
const srcTitle = (v) => SOURCE_TITLES[v]?.[isRu ? 1 : 0] ?? v;
function Content() {
    const [s, setS] = SP_REACT.useState({ up: false, receivers: [] });
    const refresh = async () => setS(await state());
    SP_REACT.useEffect(() => {
        refresh();
        const t = setInterval(refresh, 2000);
        return () => clearInterval(t);
    }, []);
    if (!s.up) {
        return (window.SP_REACT.createElement(DFL.PanelSection, { title: isRu ? "Луч" : "Luch" },
            window.SP_REACT.createElement(DFL.PanelSectionRow, null, t("notRunning"))));
    }
    return (window.SP_REACT.createElement(window.SP_REACT.Fragment, null,
        window.SP_REACT.createElement(DFL.PanelSection, { title: t("receivers") },
            s.receivers.length === 0 && window.SP_REACT.createElement(DFL.PanelSectionRow, null, t("none")),
            s.receivers.map((r) => (window.SP_REACT.createElement("div", { key: r.id },
                window.SP_REACT.createElement(DFL.PanelSectionRow, null,
                    window.SP_REACT.createElement(DFL.ButtonItem, { layout: "below", onClick: async () => {
                            if (r.mirroring)
                                await stop(r.id);
                            else
                                await start(r.id, r.source, r.size);
                            refresh();
                        }, description: r.mirroring ? t("mirroring") : r.connected ? t("connected") : r.model },
                        r.mirroring ? "■ " : "▶ ",
                        r.name)),
                window.SP_REACT.createElement(DFL.PanelSectionRow, null,
                    window.SP_REACT.createElement(DFL.DropdownItem, { label: t("mode"), rgOptions: (s.sources ?? []).map((v) => ({ data: v, label: srcTitle(v) })), selectedOption: r.source, onChange: async (o) => {
                            await choose(r.id, o.data, r.size);
                            refresh();
                        } })),
                window.SP_REACT.createElement(DFL.PanelSectionRow, null,
                    window.SP_REACT.createElement(DFL.DropdownItem, { label: t("size"), rgOptions: (s.sizes ?? []).map((v) => ({ data: v, label: v.replace("x", "×") })), selectedOption: r.size, onChange: async (o) => {
                            await choose(r.id, r.source, o.data);
                            refresh();
                        } })))))),
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
    name: "Luch",
    titleView: window.SP_REACT.createElement("div", { className: DFL.staticClasses.Title }, isRu ? "Луч" : "Luch"),
    content: window.SP_REACT.createElement(Content, null),
    icon: window.SP_REACT.createElement(FaVideo, null),
    onDismount() { },
}));

export { index as default };
//# sourceMappingURL=index.js.map
