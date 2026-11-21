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
};

const state = callable<[], State>("state");
const choose = callable<[string, string, string], void>("choose");
const start = callable<[string, string, string], void>("start");
const stop = callable<[string], void>("stop");
const setLocalAudio = callable<[boolean], void>("set_local_audio");

const SOURCE_TITLES: Record<string, string> = {
  fast: "Mirror, lowest latency",
  fast169: "Mirror without bars (16:9)",
  monitor: "Choose a screen…",
  window: "One window",
  virtual: "Second screen",
};

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
      <PanelSection title="Luch">
        <PanelSectionRow>
          Luch is not running. Start the luchd service (flatpak run dev.luch.Luch).
        </PanelSectionRow>
      </PanelSection>
    );
  }

  return (
    <>
      <PanelSection title="Receivers">
        {s.receivers.length === 0 && <PanelSectionRow>No receivers found on this network.</PanelSectionRow>}
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
                description={r.mirroring ? "Mirroring — press to stop" : r.connected ? "Connected" : r.model}
              >
                {r.mirroring ? "■ " : "▶ "}
                {r.name}
              </ButtonItem>
            </PanelSectionRow>
            <PanelSectionRow>
              <DropdownItem
                label="Mode"
                rgOptions={(s.sources ?? []).map((v) => ({ data: v, label: SOURCE_TITLES[v] ?? v }))}
                selectedOption={r.source}
                onChange={async (o) => {
                  await choose(r.id, o.data, r.size);
                  refresh();
                }}
              />
            </PanelSectionRow>
            <PanelSectionRow>
              <DropdownItem
                label="Size"
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
      <PanelSection title="Sound">
        <PanelSectionRow>
          <ToggleField
            label="Also play on this device"
            description="Off: the Deck stays silent while mirroring"
            checked={s.local_audio ?? true}
            onChange={async (on) => {
              await setLocalAudio(on);
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
  titleView: <div className={staticClasses.Title}>Luch</div>,
  content: <Content />,
  icon: <FaVideo />,
  onDismount() {},
}));
