// "Report a problem…": a short form, luchd's Report (one file in Downloads with the logs of this computer and of the
// receivers), then what to do with the file: show it (to drag into a chat), copy a description, open an issue.
// Nothing is sent anywhere by itself. The dialogs and the desktop calls come from extension.js, so this runs in tests.
// SPDX-License-Identifier: GPL-3.0-or-later

export default class Reporter {
    /**
     * @param proxy  the dev.luch.Luch1 proxy (ReportAsync)
     * @param ui     {form(): Promise<{text, hide} | null>, busy(on), done(report, [[label, callback], ...]),
     *               failed(error)}
     * @param desk   {showFile(path), copy(text), open(url)}
     * @param text   {reportShow, reportCopy, reportIssue}
     */
    constructor(proxy, ui, desk, text) {
        Object.assign(this, {proxy, ui, desk, text});
        this.running = false;
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
            this.ui.done(r, [
                [this.text.reportShow, () => this.desk.showFile(r.path)],
                [this.text.reportCopy, () => this.desk.copy(r.description)],
                [this.text.reportIssue, () => this.desk.open(r.issue)],
            ]);
        } catch (e) {
            this.ui.failed(e);
        } finally {
            this.running = false;
            this.ui.busy(false);
        }
    }
}
