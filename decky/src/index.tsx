import {
  ButtonItem,
  DropdownItem,
  PanelSection,
  PanelSectionRow,
  ToggleField,
  staticClasses,
} from "@decky/ui";
import { callable, definePlugin } from "@decky/api";
import { useEffect, useState } from "react";
import { FaVideo } from "react-icons/fa";

type Receiver = {
  id: string;
  name: string;
  model: string;
  connected: boolean;
  mirroring: boolean;
  source: string;
  size: string;
};

type State = {
  up: boolean;
  error?: string;
  receivers: Receiver[];
  sources?: string[];
  sizes?: string[];
  local_audio?: boolean;
  dim_screen?: boolean;
};

const state = callable<[], State>("state");
const choose = callable<[string, string, string], void>("choose");
const start = callable<[string, string, string], void>("start");
const stop = callable<[string], void>("stop");
const setLocalAudio = callable<[boolean], void>("set_local_audio");
const setDimScreen = callable<[boolean], void>("set_dim_screen");

// Steam UI language: Russian if the Deck is set to Russian, English otherwise.
const isRu = (() => {
  const l = (window as any).LocalizationManager?.m_rgLocalesToUse?.[0] ?? navigator.language ?? "";
  return /^(ru|russian)/i.test(String(l));
})();

const STR: Record<string, [string, string]> = {
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
const t = (k: string) => STR[k][isRu ? 1 : 0];

const SOURCE_TITLES: Record<string, [string, string]> = {
  fast: ["Fast mirror", "Быстрое зеркало"],
  fast169: ["Mirror 16:9", "Зеркало 16:9"],
  monitor: ["Pick a screen", "Выбор экрана"],
  window: ["One window", "Одно окно"],
  virtual: ["Second screen", "Второй экран"],
};
const srcTitle = (v: string) => SOURCE_TITLES[v]?.[isRu ? 1 : 0] ?? v;

function Content() {
  const [s, setS] = useState<State>({ up: false, receivers: [] });

  const refresh = async () => setS(await state());
  useEffect(() => {
    refresh();
    const t = setInterval(refresh, 2000);
    return () => clearInterval(t);
  }, []);

  if (!s.up) {
    return (
      <PanelSection title={isRu ? "Луч" : "Luch"}>
        <PanelSectionRow>
          {t("notRunning")}
        </PanelSectionRow>
      </PanelSection>
    );
  }

  return (
    <>
      <PanelSection title={t("receivers")}>
        {s.receivers.length === 0 && <PanelSectionRow>{t("none")}</PanelSectionRow>}
        {s.receivers.map((r) => (
          <div key={r.id}>
            <PanelSectionRow>
              <ButtonItem
                layout="below"
                onClick={async () => {
                  if (r.mirroring) await stop(r.id);
                  else await start(r.id, r.source, r.size);
                  refresh();
                }}
                description={r.mirroring ? t("mirroring") : r.connected ? t("connected") : r.model}
              >
                {r.mirroring ? "■ " : "▶ "}
                {r.name}
              </ButtonItem>
            </PanelSectionRow>
            <PanelSectionRow>
              <DropdownItem
                label={t("mode")}
                rgOptions={(s.sources ?? []).map((v) => ({ data: v, label: srcTitle(v) }))}
                selectedOption={r.source}
                onChange={async (o) => {
                  await choose(r.id, o.data, r.size);
                  refresh();
                }}
              />
            </PanelSectionRow>
            <PanelSectionRow>
              <DropdownItem
                label={t("size")}
                rgOptions={(s.sizes ?? []).map((v) => ({ data: v, label: v.replace("x", "×") }))}
                selectedOption={r.size}
                onChange={async (o) => {
                  await choose(r.id, r.source, o.data);
                  refresh();
                }}
              />
            </PanelSectionRow>
          </div>
        ))}
      </PanelSection>
      <PanelSection title={t("sound")}>
        <PanelSectionRow>
          <ToggleField
            label={t("alsoHere")}
            description={t("alsoHereOff")}
            checked={s.local_audio ?? true}
            onChange={async (on) => {
              await setLocalAudio(on);
              refresh();
            }}
          />
        </PanelSectionRow>
      </PanelSection>
      <PanelSection title={t("screen")}>
        <PanelSectionRow>
          <ToggleField
            label={t("dim")}
            description={t("dimOff")}
            checked={s.dim_screen ?? false}
            onChange={async (on) => {
              await setDimScreen(on);
              refresh();
            }}
          />
        </PanelSectionRow>
      </PanelSection>
    </>
  );
}

export default definePlugin(() => ({
  name: "Luch",
  titleView: <div className={staticClasses.Title}>{isRu ? "Луч" : "Luch"}</div>,
  content: <Content />,
  icon: <FaVideo />,
  onDismount() {},
}));
