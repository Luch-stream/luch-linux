"""Isolated D-Bus discovery regression check. Build target/debug/luchd first.
Requires PyGObject and dbus-daemon; --minute also checks the real 60-second timer.
Run from the repository root; the desktop Avahi and Luch services are untouched.
"""
from gi.repository import Gio, GLib
import json, os, socket, subprocess, sys, tempfile, threading, time, traceback
from pathlib import Path

def interface(name, methods, signals=''):
    return Gio.DBusNodeInfo.new_for_xml('<node><interface name="' + name + '">' + ''.join(('<method name="' + method + '">' + ''.join(('<arg type="' + t + '" direction="in"/>' for t in ins)) + ''.join(('<arg type="' + t + '" direction="out"/>' for t in outs)) + '</method>' for method, ins, outs in methods)) + signals + '</interface></node>').interfaces[0]
server = interface('org.freedesktop.Avahi.Server', [('EntryGroupNew', [], ['o']), ('ServiceBrowserNew', ['i', 'i', 's', 's', 'u'], ['o']), ('ResolveService', ['i', 'i', 's', 's', 's', 'i', 'u'], ['i', 'i', 's', 's', 's', 's', 'i', 's', 'q', 'aay', 'u'])])
group = interface('org.freedesktop.Avahi.EntryGroup', [('AddService', ['i', 'i', 'u', 's', 's', 's', 's', 'q', 'aay'], []), ('Commit', [], [])])
browser = interface('org.freedesktop.Avahi.ServiceBrowser', [('Free', [], [])])
state = {'resolves': 0, 'fail': True, 'present': True, 'connections': 0, 'browsers': {}, 'error': None}
name = 'Fake XGIMI'
rid = 'test-xgimi'
loop = GLib.MainLoop()
with tempfile.TemporaryDirectory(prefix='luch-discovery-') as tmp:
    busproc = subprocess.Popen(['dbus-daemon', '--session', '--nofork', '--print-address=1'], stdout=subprocess.PIPE, text=True)
    address = busproc.stdout.readline().strip()
    flags = Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION
    bus = Gio.DBusConnection.new_for_address_sync(address, flags, None, None)
    bus.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus', 'RequestName', GLib.Variant('(su)', ('org.freedesktop.Avahi', 0)), None, 0, -1, None)
    sock = socket.socket()
    sock.bind(('127.0.0.1', 0))
    sock.listen()
    stale = socket.socket()
    stale.bind(('127.0.0.1', 0))
    state['port'] = stale.getsockname()[1]

    def tcp():
        c, _ = sock.accept()
        state['connections'] += 1
        with c:
            f = c.makefile('rwb')
            hello = json.loads(f.readline())
            assert hello['t'] == 'hello'
            f.write(json.dumps({'t': 'welcome', 'v': 0, 'id': rid, 'name': name}).encode() + b'\n')
            f.flush()
            while True:
                line = f.readline()
                if not line:
                    break
                msg = json.loads(line)
                if msg['t'] == 'ping':
                    f.write(json.dumps({'t': 'pong', 'n': msg['n']}).encode() + b'\n')
                    f.flush()
                if msg['t'] == 'bye':
                    break
    threading.Thread(target=tcp, daemon=True).start()

    def emit(path, dest, member):
        value = GLib.Variant('(iisssu)', (1, 0, name, '_luch._tcp', 'local', 0)) if member.startswith('Item') else None
        bus.emit_signal(dest, path, 'org.freedesktop.Avahi.ServiceBrowser', member, value)

    def method(conn, sender, path, iface, member, params, inv):
        if member in ('Commit', 'AddService'):
            inv.return_value(None)
        elif member == 'EntryGroupNew':
            inv.return_value(GLib.Variant('(o)', ('/Group',)))
        elif member == 'Free':
            state['browsers'].pop(path, None)
            inv.return_value(None)
        elif member == 'ServiceBrowserNew':
            path = '/Browser' + str(time.monotonic_ns())
            state['browsers'][path] = sender
            bus.register_object(path, browser, method, None, None)
            inv.return_value(GLib.Variant('(o)', (path,)))

            def announce():
                if state['present']:
                    emit(path, sender, 'ItemNew')
                emit(path, sender, 'AllForNow')
                return False
            GLib.timeout_add(5, announce)
        elif member == 'ResolveService':
            state['resolves'] += 1
            fail = state['fail']
            port = state['port']

            def finish():
                if fail:
                    inv.return_dbus_error('org.freedesktop.Avahi.TimeoutError', 'simulated timeout')
                else:
                    result = (1, 0, name, '_luch._tcp', 'local', 'xgimi.local', 0, '127.0.0.1', port, [list(x.encode()) for x in ['v=0', 'id=' + rid, 'name=' + name]], 0)
                    inv.return_value(GLib.Variant('(' + ''.join(['i', 'i', 's', 's', 's', 's', 'i', 's', 'q', 'aay', 'u']) + ')', result))
                return False
            GLib.timeout_add(200, finish)
    bus.register_object('/', server, method, None, None)
    bus.register_object('/Group', group, method, None, None)
    env = dict(os.environ, DBUS_SYSTEM_BUS_ADDRESS=address, DBUS_SESSION_BUS_ADDRESS=address, XDG_CONFIG_HOME=tmp)
    logfile = open(Path(tmp) / 'daemon.log', 'w+')
    daemon = subprocess.Popen(['target/debug/luchd'], env=env, stdout=logfile, stderr=logfile)

    def test():
        try:
            c = Gio.DBusConnection.new_for_address_sync(address, flags, None, None)

            def call(method, args=None):
                return c.call_sync('dev.luch.Luch1', '/dev/luch/Luch1', 'dev.luch.Luch1', method, args, None, 0, 5000, None).unpack()

            def wait(pred, seconds=5):
                until = time.monotonic() + seconds
                while time.monotonic() < until:
                    if pred():
                        return
                    time.sleep(0.025)
                raise AssertionError('timed out')
            wait(lambda: state['resolves'] == 1)
            time.sleep(0.3)
            assert call('Receivers')[0] == []
            state['fail'] = False
            for _ in range(8):
                call('Refresh')
            wait(lambda: len(call('Receivers')[0]) == 1)
            assert state['resolves'] == 2, state
            assert state['connections'] == 0
            print('PASS: initial timeout recovers on Refresh; concurrent refreshes share lookup; no TCP probing', flush=True)
            state['fail'] = True
            call('Refresh')
            wait(lambda: state['resolves'] == 3)
            time.sleep(0.3)
            assert len(call('Receivers')[0]) == 1
            print('PASS: refresh timeout preserves known receiver', flush=True)
            if '--minute' in sys.argv:
                state['fail'] = False
                before = state['resolves']
                print('Checking the 60-second automatic refresh…', flush=True)
                wait(lambda: state['resolves'] > before, 70)
                time.sleep(0.3)
                assert len(call('Receivers')[0]) == 1
                print('PASS: automatic refresh fires after one minute', flush=True)
            state['fail'] = False
            old_port = state['port']
            state['port'] = sock.getsockname()[1]
            assert old_port != state['port']
            before = state['resolves']
            welcome = call('Connect', GLib.Variant('(s)', (rid,)))
            assert json.loads(welcome[0])['id'] == rid and state['resolves'] == before + 1 and (state['connections'] == 1)
            assert call('Receivers')[0][0][5] == state['port']
            call('Disconnect', GLib.Variant('(s)', (rid,)))
            print('PASS: Connect re-resolves service and performs handshake', flush=True)
            before = state['resolves']
            call('Refresh')
            wait(lambda: state['resolves'] > before)
            state['present'] = False

            def remove():
                for path, dest in list(state['browsers'].items()):
                    emit(path, dest, 'ItemRemove')
                return False
            GLib.idle_add(remove)
            wait(lambda: call('Receivers')[0] == [])
            time.sleep(0.4)
            assert call('Receivers')[0] == []
            print('PASS: ItemRemove cancels in-flight lookup; late result does not resurrect receiver', flush=True)
        except Exception:
            state['error'] = traceback.format_exc()
        finally:
            GLib.idle_add(lambda: (loop.quit(), False)[1])
    threading.Thread(target=test, daemon=True).start()
    try:
        loop.run()
    finally:
        daemon.terminate()
        daemon.wait(timeout=5)
        busproc.terminate()
        busproc.wait(timeout=5)
        sock.close()
        stale.close()
    logfile.seek(0)
    if state['error']:
        print(logfile.read())
        raise AssertionError(state['error'])
