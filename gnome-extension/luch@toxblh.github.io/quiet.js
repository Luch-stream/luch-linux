// Do not disturb while presenting or mirroring: luchd signals QuietAsk when its setting is "ask"; this asks once,
// with a notification and two buttons, and tells luchd the answer (AnswerQuiet). luchd keeps it until logout.
// SPDX-License-Identifier: GPL-3.0-or-later

export default class QuietAsker {
    /**
     * @param proxy  the dev.luch.Luch1 proxy (AnswerQuietAsync)
     * @param ask    (title, body, [[label, callback], ...]) => void: shows the question
     * @param text   {quietTitle, quietBody, quietHide, quietKeep}
     */
    constructor(proxy, ask, text) {
        Object.assign(this, {proxy, ask, text});
        this.asked = false;
        this.answer = null;
    }

    /**
     * QuietAsk arrived. Asked once a login: while the question is open nothing more is shown, and after it the
     * answer is given again by itself (luchd forgets it when it restarts).
     */
    onAsk() {
        const answer = hide => {
            this.answer = hide;
            this.proxy.AnswerQuietAsync(hide).catch(e => console.warn(`Luch: quiet: ${e}`));
        };
        if (this.answer !== null) {
            answer(this.answer);
            return;
        }
        if (this.asked)
            return;
        this.asked = true;
        this.ask(this.text.quietTitle, this.text.quietBody,
            [[this.text.quietHide, () => answer(true)], [this.text.quietKeep, () => answer(false)]]);
    }
}
