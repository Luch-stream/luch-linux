// luchd's refusals in plain words: what happened and what to do now, in Russian or English. luchd answers in short
// technical English ("receiver went away", "portal: ... Cancelled"), which says nothing to most people and nothing at
// all in a Russian menu. The kinds are the same as the Mac app's (macos/Sources/LuchEngine/Plain.swift): both are
// tested against tests/problems.json.
// SPDX-License-Identifier: GPL-3.0-or-later

/** In order: the first that matches wins. */
const KINDS = [
    // The person closed the screen picker: nothing to tell them.
    ['cancelled', /portal.*cancel|cancelled by the user/i],
    ['noService', /is luchd running|ServiceUnknown|NameHasNoOwner|was not provided by any \.service/i],
    ['sleepOff', /sleep is off on the receiver/i],
    ['sleepOld', /too old to be put to sleep/i],
    ['wrongCode', /wrong code/i],
    ['codeDigits', /the code is \d+ digits/i],
    ['codeLate', /no code was entered .* in time/i],
    ['refused', /refused the (direct )?connection/i],
    ['busy', / is busy\b/i],
    // A group: the receiver plays something of this computer's already, or cannot decode the group's picture.
    ['playingHere', / is playing already\b/i],
    ['codec', /cannot decode HEVC/i],
    ['already', /already (mirroring|connected)/i],
    ['update', /: update it\b|cannot \w+ yet: update/i],
    ['unreachable', /went away|did not answer| is not connected|channel closed|timed out|connection refused|no route to host|network is unreachable|host is down|connection reset|broken pipe/i],
];

const WORDS = {
    ru: {
        noService: 'Служба Луча на этом компьютере не запущена. Включите её: systemctl --user start luchd',
        sleepOff: 'Этот ТВ пока нельзя выключать с компьютера. На ТВ откройте Луч → Настройки → «Выключение с компьютера».',
        sleepOld: 'Этот ТВ не выключается по команде: нужен Android 9 или новее.',
        wrongCode: 'Код не подошёл. На ТВ уже новый код: повторите действие и введите его.',
        codeDigits: 'Код — это 6 цифр с экрана ТВ.',
        codeLate: 'Код не ввели вовремя. Повторите действие: ТВ покажет новый код.',
        refused: 'На ТВ отказали. Повторите и нажмите на ТВ «Разрешить».',
        busy: 'ТВ сейчас занят показом с другого устройства. Остановите его там или подождите.',
        playingHere: 'Этот ТВ уже показывает что-то с этого компьютера. Сначала остановите там показ.',
        codec: 'Этот ТВ не умеет показывать картинку в том виде, в каком её уже получают остальные. Остановите показ и начните снова, отметив и его: тогда выберется вид для всех.',
        already: 'Показ на этот ТВ уже идёт.',
        update: 'Луч на ТВ слишком старый для этого: обновите его.',
        unreachable: 'ТВ не отвечает. Проверьте, что он включён и Луч на нём открыт, и повторите.',
    },
    en: {
        noService: 'Luch’s service is not running on this computer. Start it: systemctl --user start luchd',
        sleepOff: 'This TV cannot be turned off from here yet. On the TV, open Luch › Settings › Turn off from a computer.',
        sleepOld: 'This TV cannot be turned off from here: it needs Android 9 or newer.',
        wrongCode: 'Wrong code. The TV shows a new one: do it again and type that.',
        codeDigits: 'The code is the 6 digits on the TV.',
        codeLate: 'The code was not typed in time. Do it again: the TV shows a new code.',
        refused: 'The TV said no. Try again and press Allow on the TV.',
        busy: 'The TV is showing another device. Stop that there, or wait.',
        playingHere: 'This TV is already showing something from this computer. Stop that first.',
        codec: 'This TV cannot show the picture the way the others already get it. Stop, then start again with it ticked too: a way for all is chosen then.',
        already: 'Already mirroring to this TV.',
        update: 'Luch on the TV is too old for this: update it.',
        unreachable: 'The TV does not answer. Check that it is on and Luch is open on it, then try again.',
    },
};

/** What kind of trouble a refusal is, or 'other' for one told as it is. */
export function problemKind(text) {
    const s = String(text ?? '');
    return KINDS.find(([, re]) => re.test(s))?.[0] ?? 'other';
}

/** The refusal for a person: plain words and what to do, the text itself when it is not a known kind, null when
 * there is nothing to tell (they cancelled it themselves). */
export function plainProblem(text, ru) {
    const kind = problemKind(text);
    if (kind === 'cancelled')
        return null;
    return WORDS[ru ? 'ru' : 'en'][kind] ?? String(text ?? '');
}

export const KNOWN_KINDS = KINDS.map(([kind]) => kind);
export const PROBLEM_WORDS = WORDS;
