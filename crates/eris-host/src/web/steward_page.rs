//! The Steward page: the duty board, its notes, and the forms that change them.
//!
//! Every control is a plain form posting to a dashboard route, so the page works
//! with JavaScript off. Closing a duty asks first, naming the duty, using a native
//! disclosure. A refusal (a stale revision, a closed duty) is shown on the page,
//! not in a separate error screen.

use maud::{Markup, html};

use crate::admin::{ROUTE_STEWARD_DUTY, ROUTE_STEWARD_DUTY_ADD, ROUTE_STEWARD_SEED};
use crate::db::SetupState;
use crate::steward::{Duty, FieldNote, Phase};
use crate::web::templates::{Chrome, FormError, Nav, SettingsTab, alert_region, shell};

/// Everything the Steward page needs.
#[derive(Debug, Clone)]
pub struct StewardView {
    /// The listener truth the footer states.
    pub chrome: Chrome,
    /// The daemon's state.
    pub state: SetupState,
    /// Every duty, in board order.
    pub duties: Vec<Duty>,
    /// The latest field notes, newest first.
    pub notes: Vec<FieldNote>,
    /// The default watch interval, for a duty with no cadence of its own.
    pub watch_seconds: u32,
    /// The stored charter lines, counted for the seed button.
    pub charter_lines: usize,
    /// A success line, when a form just saved.
    pub notice: Option<String>,
    /// A refusal to show above the board.
    pub error: Option<FormError>,
}

impl StewardView {
    /// An empty view for the failure path.
    #[must_use]
    pub fn new(state: SetupState) -> Self {
        Self {
            chrome: Chrome::default(),
            state,
            duties: Vec::new(),
            notes: Vec::new(),
            watch_seconds: crate::steward::DEFAULT_WATCH_SECONDS,
            charter_lines: 0,
            notice: None,
            error: None,
        }
    }
}

/// The phases a person may move a duty to from the board. `needs_you` is raised
/// only by a call, and `closed` only through the close form.
const MOVABLE: [Phase; 4] = [
    Phase::Watching,
    Phase::Working,
    Phase::Waiting,
    Phase::Paused,
];

/// The step a cadence button adds or removes, in seconds.
const CADENCE_STEP: i64 = 300;

/// The Steward page.
pub fn steward_page(view: &StewardView) -> Markup {
    let error = view.error.as_ref();
    let body = html! {
        h1 { "Steward" }
        p .lede {
            "Duties the steward keeps between conversations. Each one has a phase, a revision and "
            "a next wake. A call is a decision only you can make: the steward stops until you answer it."
        }
        (alert_region(error, view.notice.as_deref()))
        section .card aria-labelledby="steward-add" {
            h2 id="steward-add" { "New duty" }
            form method="post" action=(ROUTE_STEWARD_DUTY_ADD) novalidate {
                div .field {
                    label for="steward_statement" { "Statement" }
                    input id="steward_statement" type="text" name="statement" maxlength="400"
                        autofocus[error.is_some_and(|e| e.field.as_deref() == Some("statement"))];
                }
                div .field {
                    label for="steward_every" { "Every (seconds, blank uses the watch interval)" }
                    input id="steward_every" type="number" name="every_seconds" min="30" max="86400";
                }
                div .actions { button type="submit" { "Add duty" } }
            }
            form method="post" action=(ROUTE_STEWARD_SEED) novalidate {
                p .small {
                    (view.charter_lines) " charter line(s) on file. Seeding adds one duty per line "
                    "and skips any line that already names an open duty."
                }
                div .actions { button type="submit" { "Seed duties from charter" } }
            }
        }
        section .card aria-labelledby="steward-board" {
            h2 id="steward-board" { "Board" }
            @if view.duties.is_empty() {
                p .empty {
                    strong { "No duties yet" }
                    "Add one above, or put responsibilities in the charter on the Assistant tab and seed them."
                }
            } @else {
                table {
                    thead {
                        tr {
                            th scope="col" { "Duty" }
                            th scope="col" { "Phase" }
                            th scope="col" { "Rev" }
                            th scope="col" { "Next wake" }
                            th scope="col" { "Last note" }
                            th scope="col" { "Change" }
                        }
                    }
                    tbody {
                        @for duty in &view.duties {
                            (duty_row(view, duty))
                        }
                    }
                }
            }
            p .small { (view.duties.len()) " duty(ies)." }
        }
        section .card aria-labelledby="steward-notes" {
            h2 id="steward-notes" { "Field notes" }
            @if view.notes.is_empty() {
                p .empty { "No notes yet. The steward writes one when it has something to remember." }
            } @else {
                ul {
                    @for note in &view.notes {
                        li {
                            span .mono .small { (note.at) " " }
                            @if let Some(duty) = note.duty_id.as_deref() {
                                span .small { "[" (duty) "] " }
                            }
                            (note.text)
                        }
                    }
                }
            }
        }
        p .small {
            "Cadence, delivery and the round cap live on the Automation tab; the charter and call "
            "urgency live on Assistant. "
            a href=(SettingsTab::Automation.path()) { "Open settings" }
        }
    };
    shell(
        "Steward",
        view.state,
        Some(Nav::Steward),
        body,
        &view.chrome,
    )
}

/// One row of the board: what the duty says, where it stands, and the forms that
/// change it. Every form names the revision the row was read at.
fn duty_row(view: &StewardView, duty: &Duty) -> Markup {
    let cadence = duty.every_seconds.unwrap_or(i64::from(view.watch_seconds));
    let last_note = view
        .notes
        .iter()
        .find(|note| note.duty_id.as_deref() == Some(duty.id.as_str()))
        .map(|note| note.text.clone());
    let open_call = duty.phase == Phase::NeedsYou;
    html! {
        tr {
            td {
                (duty.statement)
                p .small .mono { (duty.id) }
                @if let Some(blocker) = duty.blocker.as_ref() {
                    p .small { "blocked: " (blocker.code) " - " (blocker.detail) }
                }
            }
            td { (duty.phase.as_str()) }
            td .mono .small { (duty.revision) }
            td .mono .small { (duty.next_wake_at.as_deref().unwrap_or("-")) }
            td .small { (last_note.as_deref().unwrap_or("-")) }
            td {
                @if duty.phase == Phase::Closed {
                    p .small { "closed: " (duty.closed_outcome.as_deref().unwrap_or("-")) }
                } @else if open_call {
                    form .inline-form method="post" action=(ROUTE_STEWARD_DUTY) novalidate {
                        input type="hidden" name="id" value=(duty.id);
                        input type="hidden" name="revision" value=(duty.revision);
                        input type="hidden" name="action" value="answer";
                        button type="submit" { "Answer: clear the call" }
                    }
                } @else {
                    form .inline-form method="post" action=(ROUTE_STEWARD_DUTY) novalidate {
                        input type="hidden" name="id" value=(duty.id);
                        input type="hidden" name="revision" value=(duty.revision);
                        input type="hidden" name="action" value="phase";
                        label for=(format!("phase-{}", duty.id)) { "Phase" }
                        select id=(format!("phase-{}", duty.id)) name="phase" {
                            @for phase in MOVABLE {
                                option value=(phase.as_str()) selected[phase == duty.phase] { (phase.as_str()) }
                            }
                        }
                        button type="submit" { "Set" }
                    }
                    form .inline-form method="post" action=(ROUTE_STEWARD_DUTY) novalidate {
                        input type="hidden" name="id" value=(duty.id);
                        input type="hidden" name="revision" value=(duty.revision);
                        input type="hidden" name="action" value="cadence";
                        span .small { "Every " (cadence) "s" }
                        button type="submit" name="delta" value=(-CADENCE_STEP) { "-" (CADENCE_STEP / 60) " min" }
                        button type="submit" name="delta" value=(CADENCE_STEP) { "+" (CADENCE_STEP / 60) " min" }
                        button type="submit" name="delta" value="default" { "Use default" }
                    }
                    details {
                        summary { "Close " (duty.statement) }
                        form .inline-form method="post" action=(ROUTE_STEWARD_DUTY) novalidate {
                            input type="hidden" name="id" value=(duty.id);
                            input type="hidden" name="revision" value=(duty.revision);
                            input type="hidden" name="action" value="close";
                            label for=(format!("outcome-{}", duty.id)) { "Outcome, in one line" }
                            input id=(format!("outcome-{}", duty.id)) type="text" name="outcome" required maxlength="400";
                            button type="submit" class="danger" { "Close duty: " (duty.statement) }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use crate::admin::{
        ROUTE_STEWARD, ROUTE_STEWARD_DUTY, ROUTE_STEWARD_DUTY_ADD, ROUTE_STEWARD_SEED,
    };
    use crate::web::harness::{Call, block_on, encode, ready_app, send};

    fn post(
        app: &axum::Router,
        cookie: &str,
        uri: &'static str,
        body: &str,
    ) -> crate::web::harness::Reply {
        block_on(send(app, Call::post(uri, body).cookie(Some(cookie))))
    }

    #[test]
    fn the_page_is_in_the_nav_and_lists_a_duty_with_its_forms() {
        let (_dir, _engine, app, cookie) = ready_app();
        let added = post(
            &app,
            &cookie,
            ROUTE_STEWARD_DUTY_ADD,
            &format!("statement={}", encode("watch the backups")),
        );
        assert_eq!(added.status, StatusCode::SEE_OTHER, "{}", added.body);
        let reply = block_on(send(
            &app,
            Call::get(ROUTE_STEWARD).html().cookie(Some(&cookie)),
        ));
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        let page = &reply.body;
        assert!(
            page.contains(&format!("href=\"{ROUTE_STEWARD}\"")),
            "the nav links the page"
        );
        assert!(page.contains("watch the backups"), "{page}");
        assert!(
            page.contains("name=\"action\" value=\"phase\""),
            "a phase form per row"
        );
        assert!(
            page.contains("name=\"action\" value=\"cadence\""),
            "a cadence stepper per row"
        );
        assert!(
            page.contains("name=\"revision\" value=\"1\""),
            "every form carries the revision it was drawn at"
        );
    }

    #[test]
    fn a_stale_revision_is_refused_on_the_page_not_on_a_blank_screen() {
        let (_dir, engine, app, cookie) = ready_app();
        post(
            &app,
            &cookie,
            ROUTE_STEWARD_DUTY_ADD,
            &format!("statement={}", encode("hold steady")),
        );
        let id = crate::steward::list_duties(engine.db(), None).expect("list")[0]
            .id
            .clone();
        // The first change lands at revision 1 -> 2.
        let first = post(
            &app,
            &cookie,
            ROUTE_STEWARD_DUTY,
            &format!("id={id}&revision=1&action=phase&phase=working"),
        );
        assert_eq!(first.status, StatusCode::SEE_OTHER, "{}", first.body);
        // A form drawn before that change still says revision 1.
        let stale = post(
            &app,
            &cookie,
            ROUTE_STEWARD_DUTY,
            &format!("id={id}&revision=1&action=phase&phase=paused"),
        );
        assert_ne!(
            stale.status,
            StatusCode::SEE_OTHER,
            "a stale write is not accepted"
        );
        assert!(
            stale.body.contains("changed since this page was drawn"),
            "{}",
            stale.body
        );
        assert!(stale.body.contains("revision 2"), "{}", stale.body);
    }

    #[test]
    fn closing_names_the_duty_and_a_closed_duty_refuses_a_second_close() {
        let (_dir, engine, app, cookie) = ready_app();
        post(
            &app,
            &cookie,
            ROUTE_STEWARD_DUTY_ADD,
            &format!("statement={}", encode("rotate the keys")),
        );
        let id = crate::steward::list_duties(engine.db(), None).expect("list")[0]
            .id
            .clone();
        let page = block_on(send(
            &app,
            Call::get(ROUTE_STEWARD).html().cookie(Some(&cookie)),
        ))
        .body;
        assert!(
            page.contains("Close rotate the keys"),
            "the confirmation names the duty"
        );
        let closed = post(
            &app,
            &cookie,
            ROUTE_STEWARD_DUTY,
            &format!("id={id}&revision=1&action=close&outcome=done"),
        );
        assert_eq!(closed.status, StatusCode::SEE_OTHER, "{}", closed.body);
        let again = post(
            &app,
            &cookie,
            ROUTE_STEWARD_DUTY,
            &format!("id={id}&revision=2&action=close&outcome=again"),
        );
        assert_ne!(again.status, StatusCode::SEE_OTHER);
        assert!(
            again.body.contains("closing is final") || again.body.contains("closed"),
            "{}",
            again.body
        );
    }

    #[test]
    fn seeding_from_the_charter_redirects_with_the_counts() {
        let (_dir, engine, app, cookie) = ready_app();
        crate::web::store::settings_put(
            engine.db(),
            crate::steward::SETTING_CHARTER,
            "Watch backups\nChase vendor\n",
        )
        .expect("charter");
        let seeded = post(&app, &cookie, ROUTE_STEWARD_SEED, "");
        assert_eq!(seeded.status, StatusCode::SEE_OTHER, "{}", seeded.body);
        let location = seeded.location().unwrap_or_default();
        assert!(location.contains("created=2"), "{location}");
        assert_eq!(
            crate::steward::list_duties(engine.db(), None)
                .expect("list")
                .len(),
            2
        );
    }
}
