import {
  ButtonItem,
  ConfirmModal,
  DropdownItem,
  PanelSection,
  PanelSectionRow,
  ProgressBar,
  ToggleField,
  showModal,
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
  codec: string;
};

type Install = { running: boolean; done: boolean; stage: number; text: string; error: string };

type State = {
  up: boolean;
  installed?: boolean;
  install?: Install;
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
const chooseCodec = callable<[string, string], void>("choose_codec");
const setLocalAudio = callable<[boolean], void>("set_local_audio");
const setDimScreen = callable<[boolean], void>("set_dim_screen");
const installLuch = callable<[], void>("install_luch");

// Steam UI language: Russian if the Deck is set to Russian, English otherwise.
const isRu = (() => {
  const l = (window as any).LocalizationManager?.m_rgLocalesToUse?.[0] ?? navigator.language ?? "";
  return /^(ru|russian)/i.test(String(l));
})();

const STR: Record<string, [string, string]> = {
  receivers: ["Receivers", "Приёмники"],
  none: ["No receivers found on this network.", "В сети не найдено приёмников."],
  notRunning: [
    "Luch is not running. Start the luchd service (flatpak run toxblh.luch.Luch).",
    "Луч не запущен. Запустите службу luchd (flatpak run toxblh.luch.Luch).",
  ],
  mirroring: ["Mirroring — press to stop", "Идёт показ — нажмите, чтобы остановить"],
  busyTitle: ["A show is already running", "Показ уже идёт"],
  busyText: [
    "Showing to %s. Stop it first, or show to both?",
    "Сейчас идёт показ на: %s. Остановить его перед новым или показывать на оба?",
  ],
  stopAndStart: ["Stop it and start", "Остановить и начать"],
  both: ["Show to both", "Показывать на оба"],
  cancel: ["Cancel", "Отмена"],
  connected: ["Connected", "Подключён"],
  mode: ["Mode", "Режим"],
  size: ["Size", "Размер"],
  codec: ["Video codec", "Видеокодек"],
  codecAuto: ["Auto", "Авто"],
  sound: ["Sound", "Звук"],
  alsoHere: ["Also play on this device", "Играть звук и на этом устройстве"],
  screen: ["Screen", "Экран"],
  dim: ["Dim the Deck screen while mirroring", "Затемнять экран Deck при показе"],
  dimOff: [
    "Brightness drops to the minimum 4 s after the menus close. Press Steam or ··· to bring it back; it also returns when the mirror stops",
    "Яркость падает до минимума через 4 с после закрытия меню. Нажмите Steam или «···», чтобы вернуть; при остановке показа она возвращается сама",
  ],
  installTitle: ["Luch is not installed yet", "Луч ещё не установлен"],
  installText: [
    "This panel needs Luch itself (a Flatpak). It is downloaded from the Luch site's release and set up here, no desktop mode needed.",
    "Панели нужен сам Луч (Flatpak). Он скачается с релиза сайта Луча и настроится прямо здесь, рабочий стол не нужен.",
  ],
  installBtn: ["Install Luch", "Установить Луч"],
  installing: ["Installing…", "Установка…"],
  installFailed: ["Could not install: ", "Не удалось установить: "],
  stage1: ["Downloading", "Скачивание"],
  stage2: ["Installing the Flatpak", "Установка Flatpak"],
  stage3: ["Starting the service", "Запуск службы"],
  stage4: ["Checking that it works", "Проверка работоспособности"],
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

  if (!s.up && s.installed === false) {
    const i = s.install;
    return (
      <PanelSection title={t("installTitle")}>
        <PanelSectionRow>{t("installText")}</PanelSectionRow>
        {i?.running && (
          <>
            <PanelSectionRow>
              {`${i.stage}/4 ${t("stage" + i.stage)} ${i.text}`}
            </PanelSectionRow>
            <PanelSectionRow>
              <ProgressBar nProgress={(i.stage - 1) * 25 + (i.stage === 1 && parseInt(i.text) ? parseInt(i.text) / 4 : 0)} />
            </PanelSectionRow>
          </>
        )}
        {i?.error && <PanelSectionRow>{t("installFailed") + i.error}</PanelSectionRow>}
        <PanelSectionRow>
          <ButtonItem layout="below" disabled={!!i?.running} onClick={() => installLuch().then(refresh)}>
            {i?.running ? t("installing") : t("installBtn")}
          </ButtonItem>
        </PanelSectionRow>
      </PanelSection>
    );
  }

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
                  showModal(
                    <ConfirmModal
                      strTitle={t("busyTitle")}
                      strDescription={t("busyText").replace("%s", others.map((x) => x.name).join(", "))}
                      strOKButtonText={t("stopAndStart")}
                      strMiddleButtonText={t("both")}
                      strCancelButtonText={t("cancel")}
                      onOK={async () => {
                        for (const o of others) await stop(o.id);
                        await start(r.id, r.source, r.size);
                        refresh();
                      }}
                      onMiddleButton={async () => {
                        await start(r.id, r.source, r.size);
                        refresh();
                      }}
                    />,
                  );
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
                label={t("codec")}
                rgOptions={[
                  { data: "auto", label: t("codecAuto") },
                  { data: "hevc", label: "HEVC (H.265)" },
                  { data: "avc", label: "H.264" },
                ]}
                selectedOption={r.codec ?? "auto"}
                onChange={async (o) => {
                  await chooseCodec(r.id, o.data);
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
