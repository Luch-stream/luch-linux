// "Report a problem…": a short form, luchd's Report (one file in Downloads with the logs of this computer and of the
// receivers), then what to do with the file: show it (to drag into a chat), copy a description, open an issue, or,
// when this luchd has a report address, send it to the developers (SendReport) and get its number to tell.
// Nothing is sent anywhere by itself. The dialogs and the desktop calls come from extension.js, so this runs in tests.
// SPDX-License-Identifier: GPL-3.0-or-later

export default class Reporter {
    /**
     * @param proxy  the dev.luch.Luch1 proxy (ReportAsync, SendReportAsync)
     * @param ui     {form(): Promise<{text, hide} | null>, busy(on), done(report, [[label, callback], ...]) → view
     *               {say(text, [[label, callback], ...])}, failed(error)}
     * @param desk   {showFile(path), copy(text), open(url)}
     * @param text   {reportShow, reportCopy, reportIssue, reportSend, reportSending, reportSent(number),
     *               reportSentNoFile(number), reportNotSent(error), reportCopyNumber}
     */
    constructor(proxy, ui, desk, text) {
        Object.assign(this, {proxy, ui, desk, text});
        this.running = false;
        this.sending = false;
    }

    /** The whole of it, from the form; a second press while a report is collected does nothing. */
    async run() {
        if (this.running)
            return;
        this.running = true;
        try {
            const answer = await this.ui.form();
            if (!answer)
                return;
            this.ui.busy(true);
            const [json] = await this.proxy.ReportAsync(answer.text.trim(), !answer.hide);
            const r = JSON.parse(json);
            const view = this.ui.done(r, this._actions(r, () => view));
        } catch (e) {
            this.ui.failed(e);
        } finally {
            this.running = false;
            this.ui.busy(false);
        }
    }

    /** The file's ways out; Send only when luchd can send (it says so in the report). */
    _actions(r, view) {
        const ways = [
            [this.text.reportShow, () => this.desk.showFile(r.path)],
            [this.text.reportCopy, () => this.desk.copy(r.description)],
            [this.text.reportIssue, () => this.desk.open(r.issue)],
        ];
        if (r.can_send)
            ways.push([this.text.reportSend, () => this.send(r, view())]);
        return ways;
    }

    /** Sends the report shown in `view`: "Sending…", then its number with Copy, or why not with the file's ways. */
    async send(r, view) {
        if (this.sending)
            return;
        this.sending = true;
        view.say(this.text.reportSending, []);
        try {
            const [json] = await this.proxy.SendReportAsync(r.path);
            const s = JSON.parse(json);
            view.say(s.attached === false ? this.text.reportSentNoFile(s.number) : this.text.reportSent(s.number),
                [[this.text.reportCopyNumber, () => this.desk.copy(s.number)]]);
        } catch (e) {
            view.say(this.text.reportNotSent(e), this._actions(r, () => view));
        } finally {
            this.sending = false;
        }
    }
}
