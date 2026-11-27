// The presenter view's logic (luchd present.rs serves it): keys, the pointer, ink, the timer. Plain functions and
// small classes with the clock passed in, so node tests run them without a browser; start() wires them to the page.
// SPDX-License-Identifier: GPL-3.0-or-later

/** What a key does. Clickers send PageUp/PageDown, B or "." (black screen) and F5/Escape (their start button). */
export function keyAction(e) {
    if ((e.ctrlKey || e.metaKey) && (e.key === 'z' || e.key === 'Z')) return 'undo';
    if (e.ctrlKey || e.metaKey || e.altKey) return null;
    switch (e.key) {
    case 'PageDown': case 'ArrowRight': case 'ArrowDown': case ' ': case 'Enter': case 'n': case 'N':
        return 'next';
    case 'PageUp': case 'ArrowLeft': case 'ArrowUp': case 'Backspace': case 'p': case 'P':
        return 'prev';
    case 'Home': return 'first';
    case 'End': return 'last';
    case 'b': case 'B': case '.': return 'blank';
    // A clicker's start button sends F5 and Escape in turn: both bring the picture back, neither leaves.
    case 'F5': case 'Escape': return 'unblank';
    case 'l': case 'L': return 'laser';
    case 'c': case 'C': return 'clear';
    case 't': case 'T': return 'pause';
    case 'r': case 'R': return 'reset';
    case '+': case '=': return 'bigger';
    case '-': return 'smaller';
    default: return null;
    }
}

/** The page to go to for an action, or null. */
export function targetPage(action, page, pages) {
    const to = {next: page + 1, prev: page - 1, first: 0, last: pages - 1}[action];
    if (to === undefined || pages <= 0) return null;
    const clamped = Math.max(0, Math.min(pages - 1, to));
    return clamped === page ? null : clamped;
}

const round = v => Math.round(Math.max(0, Math.min(1, v)) * 10000) / 10000;

/** A point on the screen as fractions of the picture's rectangle; null when it is outside. */
export function normalize(clientX, clientY, rect) {
    if (!rect || rect.width <= 0 || rect.height <= 0) return null;
    const x = (clientX - rect.left) / rect.width, y = (clientY - rect.top) / rect.height;
    if (x < 0 || x > 1 || y < 0 || y > 1) return null;
    return {x: round(x), y: round(y)};
}

/** Where a picture of natural size nw×nh is drawn in a box with object-fit: contain. */
export function contentRect(box, nw, nh) {
    if (!nw || !nh || box.width <= 0 || box.height <= 0) return box;
    const k = Math.min(box.width / nw, box.height / nh);
    const width = nw * k, height = nh * k;
    return {left: box.left + (box.width - width) / 2, top: box.top + (box.height - height) / 2, width, height};
}

/** "4:05", "1:02:03". */
export function formatClock(ms) {
    const s = Math.max(0, Math.floor(ms / 1000));
    const h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60, sec = s % 60;
    const two = n => String(n).padStart(2, '0');
    return h > 0 ? `${h}:${two(m)}:${two(sec)}` : `${m}:${two(sec)}`;
}

/** The talk's elapsed time: runs from the start, can be paused and reset. */
export class Timer {
    constructor(now) {
        this.now = now;
        this.reset();
    }

    reset() {
        this.base = 0;
        this.since = this.now();
    }

    get running() {
        return this.since !== null;
    }

    toggle() {
        if (this.running) {
            this.base += this.now() - this.since;
            this.since = null;
        } else {
            this.since = this.now();
        }
    }

    elapsed() {
        return this.base + (this.running ? this.now() - this.since : 0);
    }
}

/**
 * Pointer moves to luchd at most every `every` ms: the first at once, then the newest when its turn comes; the
 * last one always goes. A pointer that trails behind the hand is worse than one that skips.
 */
export class PointerCoalescer {
    constructor(send, {every = 16, now, later}) {
        Object.assign(this, {send, every, now, later});
        this.last = -Infinity;
        this.pending = null;
        this.timer = null;
    }

    offer(msg) {
        const t = this.now();
        if (this.pending === null && t - this.last >= this.every) {
            this.last = t;
            this.send(msg);
            return;
        }
        this.pending = msg;
        if (this.timer === null) this.timer = this.later(() => this.flush(), Math.max(0, this.last + this.every - t));
    }

    flush() {
        this.timer = null;
        if (this.pending === null) return;
        this.last = this.now();
        const m = this.pending;
        this.pending = null;
        this.send(m);
    }
}

/**
 * A stroke being drawn, sent in parts every `every` ms so the TV draws it as the hand moves (PROTOCOL.md 4h).
 * Points nearer than `minStep` to the previous one add nothing and are left out.
 */
export class StrokeSender {
    constructor(send, {every = 30, now, later, minStep = 0.0015, max = 2000}) {
        Object.assign(this, {send, every, now, later, minStep, max});
        this.id = 0;
        this.open = null;
    }

    begin(id, p, color, width) {
        this.end();
        this.open = {id, buf: [p.x, p.y], last: p, timer: null};
        this.send({t: 'ink', do: 'stroke', id, color, width, points: this.open.buf});
        this.open.buf = [];
    }

    add(p) {
        const o = this.open;
        if (!o || Math.hypot(p.x - o.last.x, p.y - o.last.y) < this.minStep) return;
        o.last = p;
        o.buf.push(p.x, p.y);
        if (o.buf.length >= 2 * this.max) this.flush();
        else if (o.timer === null) o.timer = this.later(() => this.flush(), this.every);
    }

    flush() {
        const o = this.open;
        if (!o) return;
        o.timer = null;
        if (o.buf.length === 0) return;
        this.send({t: 'ink', do: 'stroke', id: o.id, points: o.buf});
        o.buf = [];
    }

    end() {
        this.flush();
        this.open = null;
    }
}

/** The ink of each page as the presenter sees it; the same rules as the receiver's (undo, clear per page). */
export class InkPages {
    constructor() {
        this.pages = new Map();
    }

    strokes(page) {
        if (!this.pages.has(page)) this.pages.set(page, []);
        return this.pages.get(page);
    }

    add(page, stroke) {
        this.strokes(page).push(stroke);
    }

    undo(page) {
        this.strokes(page).pop();
    }

    clear(page) {
        this.pages.set(page, []);
    }
}

export const COLORS = [
    {name: 'red', color: '#e53935', width: 0.004},
    {name: 'yellow', color: '#fdd835', width: 0.004},
    {name: 'green', color: '#43a047', width: 0.004},
    {name: 'blue', color: '#1e88e5', width: 0.004},
    {name: 'white', color: '#ffffff', width: 0.004},
    {name: 'marker', color: '#fff17660', width: 0.02},
];

const RU = (globalThis.navigator?.language ?? '').startsWith('ru');
export const T = RU ? {
    next: 'Следующий', notes: 'Заметки', noNotes: 'Нет заметок', ended: 'Показ окончен', offline: 'Нет связи с luchd',
    blank: 'Чёрный экран', laser: 'Указка', undo: 'Отменить', clear: 'Стереть', reset: 'Сбросить', last: 'Последний слайд',
    noPicture: 'Нет картинки: установите poppler-utils',
} : {
    next: 'Next', notes: 'Notes', noNotes: 'No notes', ended: 'The presentation is over', offline: 'Lost luchd',
    blank: 'Black screen', laser: 'Laser', undo: 'Undo', clear: 'Clear', reset: 'Reset', last: 'Last slide',
    noPicture: 'No picture: install poppler-utils',
};

/** Draws strokes (fractions of the picture) on a 2D canvas over rectangle `r` (canvas pixels). */
export function drawStrokes(ctx, strokes, r) {
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';
    for (const s of strokes) {
        const pts = s.points;
        if (pts.length < 2) continue;
        ctx.strokeStyle = s.color;
        ctx.fillStyle = s.color;
        ctx.lineWidth = Math.max(1, s.width * r.width);
        const X = i => r.left + pts[i] * r.width, Y = i => r.top + pts[i + 1] * r.height;
        ctx.beginPath();
        if (pts.length === 2) {
            ctx.arc(X(0), Y(0), ctx.lineWidth / 2, 0, 2 * Math.PI);
            ctx.fill();
            continue;
        }
        ctx.moveTo(X(0), Y(0));
        // Through the midpoints, as the receiver draws them, so both look the same.
        for (let i = 2; i < pts.length - 2; i += 2) {
            ctx.quadraticCurveTo(X(i), Y(i), (X(i) + X(i + 2)) / 2, (Y(i) + Y(i + 2)) / 2);
        }
        ctx.lineTo(X(pts.length - 2), Y(pts.length - 2));
        ctx.stroke();
    }
}

/** Wires everything to the page. */
export function start(win, doc) {
    const $ = id => doc.getElementById(id);
    const now = () => win.performance.now();
    const later = (fn, ms) => win.setTimeout(fn, ms);
    const st = {page: 0, pages: 0, blank: false, notes: [], laser: true, pen: COLORS[0], ended: false, ink: true};
    const ink = new InkPages();
    const timer = new Timer(() => Date.now());
    let ws = null;
    const send = m => {
        if (ws && ws.readyState === 1) ws.send(JSON.stringify(m));
    };
    const pointer = new PointerCoalescer(send, {now, later});
    const strokes = new StrokeSender(send, {now, later});
    let strokeId = Math.floor(Date.now() % 1e9);
    let noteSize = 22;
    try {
        noteSize = Number(win.localStorage.getItem('luch-note-size')) || 22;
    } catch (e) { /* private window: the default */ }

    const slide = $('current'), next = $('next'), canvas = $('ink');
    const picture = () => contentRect(slide.getBoundingClientRect(), slide.naturalWidth, slide.naturalHeight);
    const width = el => Math.round(el.getBoundingClientRect().width * (win.devicePixelRatio || 1));

    function redraw() {
        const dpr = win.devicePixelRatio || 1, box = canvas.getBoundingClientRect();
        canvas.width = Math.round(box.width * dpr);
        canvas.height = Math.round(box.height * dpr);
        const ctx = canvas.getContext('2d');
        ctx.clearRect(0, 0, canvas.width, canvas.height);
        if (st.blank) return;
        const r = picture();
        drawStrokes(ctx, ink.strokes(st.page), {left: (r.left - box.left) * dpr, top: (r.top - box.top) * dpr,
            width: r.width * dpr, height: r.height * dpr});
    }

    function render() {
        $('counter').textContent = st.pages ? `${st.page + 1} / ${st.pages}` : '';
        slide.src = `slide/${st.page}.png?w=${width(slide)}`;
        if (st.page + 1 < st.pages) {
            next.src = `slide/${st.page + 1}.png?w=${width(next)}`;
            next.hidden = false;
            $('last').hidden = true;
        } else {
            next.hidden = true;
            $('last').hidden = false;
        }
        const note = st.notes[st.page] ?? '';
        $('notes').textContent = note || T.noNotes;
        $('notes').classList.toggle('empty', !note);
        $('notes').style.fontSize = `${noteSize}px`;
        doc.body.classList.toggle('blank', st.blank);
        doc.body.classList.toggle('ended', st.ended);
        $('laser').classList.toggle('on', st.laser);
        for (const b of doc.querySelectorAll('[data-pen]')) b.classList.toggle('on', b.dataset.pen === st.pen.name);
        redraw();
    }

    function act(a) {
        const to = targetPage(a, st.page, st.pages);
        if (to !== null) {
            send({t: 'doc_page', page: to});
            st.page = to; // the receiver confirms with its own doc_page
            st.blank = false;
        } else if (a === 'blank' || a === 'unblank') {
            st.blank = a === 'blank' ? !st.blank : false;
            send({t: 'blank', on: st.blank});
        } else if (a === 'undo') {
            ink.undo(st.page);
            send({t: 'ink', do: 'undo'});
        } else if (a === 'clear') {
            ink.clear(st.page);
            send({t: 'ink', do: 'clear'});
        } else if (a === 'laser') {
            st.laser = !st.laser;
            if (!st.laser) send({t: 'pointer', show: false});
        } else if (a === 'pause') {
            timer.toggle();
        } else if (a === 'reset') {
            timer.reset();
        } else if (a === 'bigger' || a === 'smaller') {
            noteSize = Math.max(12, Math.min(64, noteSize + (a === 'bigger' ? 2 : -2)));
            try {
                win.localStorage.setItem('luch-note-size', String(noteSize));
            } catch (e) { /* not kept */ }
        } else {
            return;
        }
        render();
    }

    doc.addEventListener('keydown', e => {
        const a = keyAction(e);
        if (!a) return;
        e.preventDefault(); // F5 must not reload the page, Space must not scroll
        act(a);
    });

    // Over the slide: moving is the laser, dragging draws, a click turns to the next slide, a right click back.
    let down = null;
    slide.parentElement.addEventListener('pointermove', e => {
        const p = normalize(e.clientX, e.clientY, picture());
        if (down && !down.drawing && Math.hypot(e.clientX - down.cx, e.clientY - down.cy) > 4 && st.ink) {
            down.drawing = {id: ++strokeId, color: st.pen.color, width: st.pen.width, points: [down.p.x, down.p.y]};
            ink.add(st.page, down.drawing);
            if (st.laser) pointer.offer({t: 'pointer', show: false});
            strokes.begin(down.drawing.id, down.p, st.pen.color, st.pen.width);
        }
        if (down?.drawing && p) {
            down.drawing.points.push(p.x, p.y);
            strokes.add(p);
            redraw();
        } else if (!down && st.laser && !st.blank) {
            pointer.offer(p ? {t: 'pointer', x: p.x, y: p.y} : {t: 'pointer', show: false});
        }
    });
    slide.parentElement.addEventListener('pointerdown', e => {
        if (e.button !== 0) return;
        const p = normalize(e.clientX, e.clientY, picture());
        if (!p) return;
        down = {cx: e.clientX, cy: e.clientY, p, drawing: null};
        slide.parentElement.setPointerCapture?.(e.pointerId);
    });
    slide.parentElement.addEventListener('pointerup', e => {
        if (!down) return;
        if (down.drawing) strokes.end();
        else if (e.button === 0) act('next');
        down = null;
    });
    slide.parentElement.addEventListener('pointerleave', () => {
        if (st.laser && !down) pointer.offer({t: 'pointer', show: false});
    });
    slide.parentElement.addEventListener('contextmenu', e => {
        e.preventDefault();
        act('prev');
    });
    for (const b of doc.querySelectorAll('[data-act]')) b.addEventListener('click', () => act(b.dataset.act));
    for (const b of doc.querySelectorAll('[data-pen]')) {
        b.addEventListener('click', () => {
            st.pen = COLORS.find(c => c.name === b.dataset.pen) ?? COLORS[0];
            render();
        });
    }
    slide.addEventListener('load', redraw);
    slide.addEventListener('error', () => { $('nopicture').hidden = false; });
    win.addEventListener('resize', render);
    win.setInterval(() => {
        $('timer').textContent = formatClock(timer.elapsed());
        $('timer').classList.toggle('paused', !timer.running);
        const d = new Date();
        $('clock').textContent = `${d.getHours()}:${String(d.getMinutes()).padStart(2, '0')}`;
    }, 250);

    function connect() {
        ws = new win.WebSocket(`ws://${win.location.host}${win.location.pathname}ws`);
        ws.onmessage = ev => {
            const m = JSON.parse(ev.data);
            if (m.t === 'hello') {
                Object.assign(st, {page: m.page, pages: m.pages, blank: m.blank, notes: m.notes ?? [], ink: m.ink});
                $('title').textContent = `${m.name} · ${m.receiver}`;
                doc.title = m.name;
                doc.body.classList.toggle('noink', !m.ink);
            } else if (m.t === 'state') {
                if (m.page !== st.page) strokes.end();
                Object.assign(st, {page: m.page, pages: m.pages, blank: m.blank});
            } else if (m.t === 'ended') {
                st.ended = true;
            }
            render();
        };
        ws.onclose = () => {
            if (!st.ended) {
                doc.body.classList.add('offline');
                win.setTimeout(() => win.location.reload(), 3000);
            }
        };
    }
    $('offline').textContent = T.offline;
    $('endedText').textContent = T.ended;
    $('last').textContent = T.last;
    $('nopicture').textContent = T.noPicture;
    connect();
}
