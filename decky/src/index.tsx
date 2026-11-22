import {
  ButtonItem,
  ConfirmModal,
  DialogButton,
  DropdownItem,
  Focusable,
  PanelSection,
  PanelSectionRow,
  ProgressBar,
  ToggleField,
  showModal,
  staticClasses,
} from "@decky/ui";
import { callable, definePlugin } from "@decky/api";
import { useEffect, useState } from "react";

// The Luch mark as an outline, the same as the one in the Plasma panel widget (luch-outline.svg), in the colour of the text.
const LuchIcon = () => (
  <svg width="1em" height="1em" viewBox="0 0 22 22" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
    <rect x="3.5" y="2.5" width="15" height="9" rx="1.5" />
    <path d="M7.5 14.5 L5.5 12.5 M14.5 14.5 L16.5 12.5" />
    <rect x="7.5" y="14.5" width="7" height="4" rx="1" />
    <path d="M6 20.5 H16" />
  </svg>
);

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
const startService = callable<[], boolean>("start_service");

import { lang, t } from "./i18n";
import { qrFor, qrPath } from "./qr";

const srcTitle = (v: string) => t(v);

// A row of text with air around it (the panel packs its rows tightly).
const Gap = ({ children, below = 10, size, dim }: { children: any; below?: number; size?: number; dim?: boolean }) => (
  <PanelSectionRow>
    <div style={{ paddingBottom: below, fontSize: size, opacity: dim ? 0.7 : 1 }}>{children}</div>
  </PanelSectionRow>
);

// 1920x1080 -> 1080p
const short = (size: string) => (/^\d+x(\d+)$/.exec(size) ? `${/^\d+x(\d+)$/.exec(size)![1]}p` : size);

function Content() {
  const [s, setS] = useState<State>({ up: false, receivers: [] });
  // luchd takes a few seconds after a start or an install: a loading bar first, the hint only when it does not come up
  const [downFor, setDownFor] = useState(0);
  // the mode, codec and size of a receiver are folded away until its arrow is pressed
  const [open, setOpen] = useState<Record<string, boolean>>({});
  const [qrOpen, setQrOpen] = useState(false);

  const refresh = async () => setS(await state());
  useEffect(() => {
    refresh();
    const t = setInterval(refresh, 2000);
    return () => clearInterval(t);
  }, []);
  useEffect(() => {
    setDownFor((d) => (s.up ? 0 : d + 2));
  }, [s]);

  if (!s.up && s.installed === false) {
    const i = s.install;
    const pct = i?.stage === 1 && parseInt(i.text) ? parseInt(i.text) / 4 : 0;
    return (
      <PanelSection title={t("installTitle")}>
        <Gap size={12} dim>{t("tagline")}</Gap>
        <Gap below={16}>{t("installText")}</Gap>
        {i?.running && (
          <>
            <Gap below={8}>{`${i.stage}/4 · ${t("stage" + i.stage)} ${i.text}`}</Gap>
            <Gap below={16}>
              <ProgressBar nProgress={i.stage > 1 ? (i.stage - 1) * 25 + pct : pct} />
            </Gap>
          </>
        )}
        {i?.error && <Gap below={16}>{t("installFailed") + (i.error === "download" ? t("errDownload") : i.error)}</Gap>}
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
      <PanelSection title={t("appName")}>
        {downFor < 30 ? (
          <>
            <Gap below={10}>{t("starting")}</Gap>
            <Gap below={10}>
              <ProgressBar indeterminate nProgress={0} />
            </Gap>
          </>
        ) : (
          <>
            <Gap below={12}>{t("notRunning")}</Gap>
            <PanelSectionRow>
              <ButtonItem layout="below" onClick={() => startService().then(refresh)}>
                {t("startBtn")}
              </ButtonItem>
            </PanelSectionRow>
          </>
        )}
      </PanelSection>
    );
  }

  return (
    <>
      <PanelSection title={t("receivers")}>
        {s.receivers.length === 0 && <Gap>{t("none")}</Gap>}
        {s.receivers.map((r) => (
          <div key={r.id} style={{ paddingBottom: 14 }}>
            <PanelSectionRow>
              <Focusable style={{ display: "flex", gap: "8px", alignItems: "stretch" }} flow-children="horizontal">
                <DialogButton
                  style={{ flex: 1, minWidth: 0, textAlign: "left", padding: "10px 14px" }}
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
                >
                  <div style={{ fontWeight: 600 }}>
                    {r.mirroring ? "■ " : "▶ "}
                    {r.name}
                  </div>
                  <div style={{ fontSize: "12px", opacity: 0.7, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", marginTop: 2 }}>
                    {r.mirroring ? t("mirroring") : `${srcTitle(r.source)} · ${short(r.size)}`}
                  </div>
                </DialogButton>
                <DialogButton
                  style={{ width: "44px", minWidth: "44px", padding: "0", height: "auto" }}
                  onClick={() => setOpen((o) => ({ ...o, [r.id]: !o[r.id] }))}
                >
                  {open[r.id] ? "▴" : "▾"}
                </DialogButton>
              </Focusable>
            </PanelSectionRow>
            {open[r.id] && (
              <>
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
              </>
            )}
          </div>
        ))}
        <PanelSectionRow>
          <DialogButton
            style={{ width: "100%", textAlign: "left", padding: "10px 14px", display: "flex", justifyContent: "space-between", alignItems: "center", gap: 8 }}
            onClick={() => setQrOpen((o) => !o)}
          >
            <span style={{ fontSize: "13px" }}>{t("getReceiver")}</span>
            <span>{qrOpen ? "▴" : "▾"}</span>
          </DialogButton>
        </PanelSectionRow>
        {qrOpen && (
          <>
            <Gap below={10} size={12} dim>{t("getReceiverText")}</Gap>
            <PanelSectionRow>
              <div style={{ display: "flex", justifyContent: "center", paddingBottom: 6 }}>
                <svg
                  width="124"
                  height="124"
                  viewBox="-4 -4 41 41"
                  shapeRendering="crispEdges"
                  style={{ background: "#fff", borderRadius: 6 }}
                >
                  <path d={qrPath(qrFor(lang).rows)} fill="#000" />
                </svg>
              </div>
            </PanelSectionRow>
            <Gap below={4} size={11} dim>{qrFor(lang).url.replace("https://", "")}</Gap>
          </>
        )}
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
  name: t("appName"),
  titleView: <div className={staticClasses.Title}>{t("appName")}</div>,
  content: <Content />,
  icon: <LuchIcon />,
  onDismount() {},
}));
