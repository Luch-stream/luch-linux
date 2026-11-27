# Input from the receiver

A tablet showing this computer's desktop can be touched like a touch screen, and a keyboard, mouse or gamepad
plugged into a TV can drive the computer. The wire format is PROTOCOL.md section 4h (in luch-android); this page
says how `luchd` does it and why.

## What others do

- **Moonlight / Sunshine.** Input rides the ENet control stream: one ENet channel per device class (mouse,
  keyboard, each gamepad, touch, pen). Mouse, keyboard and gamepad state are sent reliably. Touch and pen hover and
  motion are sent unreliably, since the next sample replaces them. Packet types: relative and absolute mouse (the
  absolute one carries the reference size), buttons, a 120-per-notch wheel, keys as Windows VK codes, a
  multi-controller state (buttons, two triggers, four stick axes) and Sunshine's own touch and pen packets
  (pointer id, normalized x/y floats, pressure or hover distance, contact area, rotation, tilt). Both ends
  coalesce. Relative motion and scroll are summed, an absolute position or a pad state keeps only the newest
  sample, and pen motion is batched at 1 ms. On Linux Sunshine injects through inputtino (uinput, uhid), which
  needs write access to `/dev/uinput`.
- **Miracast UIBC.** A TCP channel negotiated over RTSP (`wfd_uibc_capability`). "Generic" input (touch down,
  move and up with a pointer id, keys, zoom, scroll, rotate) has its coordinates scaled to the negotiated
  resolution. "HIDC" input passes HID reports through.
- **Steam Link / Remote Play.** The host makes virtual controllers with uinput through Steam Input. The
  `steam-devices` udev rules give the logged-in user access to `/dev/uinput`.
- **The RemoteDesktop portal.** `NotifyPointerMotion` / `MotionAbsolute` / `Button` / `Axis(finish)` /
  `AxisDiscrete`, `NotifyKeyboardKeycode` (evdev codes) and `NotifyTouchDown` / `Motion` / `Up` (slot, x and y in
  the stream's logical pixels, naming the stream's PipeWire node). Version 2 adds `persist_mode` and
  `restore_token` in `SelectDevices`, and `ConnectToEIS`, a libei socket that replaces the Notify calls. The
  portal has keyboard, pointer and touch screen, but no tablet with pressure and no gamepad. A RemoteDesktop
  session can carry a ScreenCast (`ScreenCast.SelectSources` on the same session), so one dialog approves both.
  GNOME (EIS since 45) and KDE (touch, EIS through KWin) implement it. niri, xdg-desktop-portal-wlr and Hyprland
  have no RemoteDesktop.

## The design and why

- **The portal first, combined with the screencast.** It needs no root and no udev rule, and it works on GNOME
  and KDE under Wayland. Touch is real multitouch on the very stream that is shown, so the compositor maps it to
  the right monitor or window. The person approves the picture and the control in one dialog, and the restore
  token (`~/.config/luch/restore-token-remote*`) means no dialog next time. A desktop without that portal, or a
  person who turns control off in the dialog, still gets the plain screencast.
- **uinput where the portal has nothing**: gamepads (an Xbox 360 layout, 045e:028e, which games and Steam know
  without a mapping), a stylus with pressure (a direct tablet), and keyboard, mouse and touch on niri and wlroots
  (absolute positions as a fraction of the whole desktop; niri's output layout comes from `niri msg -j outputs`).
  It is used only when `/dev/uinput` is writable for the user (`input` group or `steam-devices`). luchd never
  asks for root.
- **D-Bus Notify calls, not libei yet.** A Notify call on the session bus costs about 0.1 ms, far below a frame,
  and needs no new library. `ConnectToEIS` is the next step once a Rust libei crate is worth the dependency.
  After that call the Notify methods may not be used.
- **UDP on the video flow, not a new socket.** The receiver sends from its video socket to the address and port
  the video comes from. The computer's firewall lets that in as the answer of a flow it opened (no port to open
  for firewalld or ufw), `ready` needs no new port, and the VPN binding of the video socket covers it. The socket
  is connected, so the kernel already drops datagrams from any other address and port.
- **State, not events.** Every datagram carries the full touch state, running totals of pointer motion and wheel,
  numbered key events repeated for 300 ms with a held list, and gamepad state. A lost datagram costs one
  in-between position and never leaves a stuck key or a lost click. No acknowledgements, no retransmission
  delay. Sunshine needs ENet's reliability for keys; this needs none.
- **Coalescing**: the receiver sends on every change, the newest state replacing one not yet sent. luchd's queue
  is 64 datagrams deep and drops when the injector falls behind, since the next state supersedes them anyway.
- **The person decides.** The first use asks with a notification: "May Living room TV control this computer?"
  (allow once / always / don't allow; no answer in 60 s is a no). "Always" is kept in `~/.config/luch/input` per
  receiver id and can be taken back (`SetInputChoice(id, "ask")`). Nothing is injected before a grant. Each
  datagram carries a random per-session token. It stops blind injection on the LAN, but it travels in the clear
  like the whole control channel.
- **The watchdog**: no datagram for 500 ms while anything is held lets everything go.

## Where a point lands

The receiver sends fractions of the picture (0…65535). For the portal that becomes logical pixels of the stream
(`size` from the portal: a 4K monitor at 150 % is 2560×1440). For uinput it becomes a fraction of the bounding
box of all outputs, which needs the captured area's position in the layout (`map.rs`, with tests for a
monitor beside the laptop and outputs at negative coordinates). A window stream's position is unknown to uinput,
so windows take input through the portal only.

## Switches

`LUCH_INPUT=off` turns input off. `uinput` skips the portal and uses uinput only. `portal` never uses uinput.

## Not done

- A phone as the remote of a TV session (the reverse direction), input from the web receiver, and an Android
  sender obeying input (it would need an accessibility service).
- `fast` (KMS) takes input only through uinput. A RemoteDesktop session without a screencast has no stream for
  absolute positions.
- KDE persists its sessions with a `restore_data` of its own instead of `restore_token`. If no token comes back,
  KDE asks each time.

## Tests

`cargo test -p luchd input` (under `dbus-run-session`) covers:

- packet parsing;
- the tracker: loss, wrap-around, the watchdog;
- the coordinate mapping;
- uinput's event sequences and ioctl numbers;
- the permission steps and the notification;
- a fake xdg-desktop-portal on the private bus, played the whole way: UDP datagram, gate, tracker, injector,
  ashpd, and the portal calls with their arguments.

`docker run --privileged … cargo test -p luchd uinput_device -- --ignored` makes real uinput devices and reads a
gamepad's events back from the kernel.
