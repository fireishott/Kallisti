//! The permission matrix: defaults, the closed option sets, and the one-line
//! consequence each row carries.
//!
//! The defaults matter more than the form. Every value here has a safe default,
//! every set is closed (an unknown string is a refusal, not a new mode), and
//! two rows are locked rather than configurable because 1.0 has no
//! implementation behind them. A UI that offered them would be lying.
//!
//! The rule that is not a preference: onboarding cannot select `full` sandbox
//! mode. Only `eris serve --allow-full` at process start can, so the choice
//! survives a browser session, a copied config, and a tired operator.

use std::fmt;

/// What a turn may touch. Closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    /// Read the workspace, nothing else.
    ReadOnly,
    /// Read and write inside the workspace only.
    WorkspaceWrite,
    /// Unrestricted. Not reachable from onboarding by design.
    Full,
}

impl SandboxMode {
    /// The default: the narrowest mode that still does something useful.
    pub const DEFAULT: Self = Self::ReadOnly;

    /// Every option, in display order. `Full` is listed so the row can explain
    /// why it is not selectable here.
    pub const ALL: [Self; 3] = [Self::ReadOnly, Self::WorkspaceWrite, Self::Full];

    /// Options onboarding may offer.
    pub const ONBOARDING: [Self; 2] = [Self::ReadOnly, Self::WorkspaceWrite];

    /// The wire and database spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::Full => "full",
        }
    }

    /// The label a person reads.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read only",
            Self::WorkspaceWrite => "Workspace write",
            Self::Full => "Full host",
        }
    }

    /// One line of plain English: what changes if you pick this.
    #[must_use]
    pub fn consequence(self) -> &'static str {
        match self {
            Self::ReadOnly => {
                "Turns can read the workspace but cannot write a file, run a command, or open a socket."
            }
            Self::WorkspaceWrite => {
                "Turns can create and edit files inside the workspace, and nothing outside it."
            }
            Self::Full => {
                "Turns can run commands and touch the whole host. Only a restart of eris with --allow-full can select this."
            }
        }
    }

    /// Parse a stored or submitted value.
    pub fn parse(value: &str) -> Result<Self, PermissionError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "read-only" | "read only" | "readonly" => Ok(Self::ReadOnly),
            "workspace-write" | "workspace write" | "workspacewrite" => Ok(Self::WorkspaceWrite),
            "full" => Ok(Self::Full),
            other => Err(PermissionError::UnknownValue {
                field: "sandbox_mode",
                value: other.to_owned(),
            }),
        }
    }
}

impl fmt::Display for SandboxMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether an action needs a human first. Closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Everything that leaves the sandbox waits.
    Ask,
    /// In-sandbox work runs unattended.
    Never,
}

impl ApprovalPolicy {
    /// The default: ask.
    pub const DEFAULT: Self = Self::Ask;

    /// Every option, in display order.
    pub const ALL: [Self; 2] = [Self::Ask, Self::Never];

    /// The wire and database spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Never => "never",
        }
    }

    /// The label a person reads.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "Ask me",
            Self::Never => "Never ask",
        }
    }

    /// One line of plain English: what changes if you pick this.
    #[must_use]
    pub fn consequence(self) -> &'static str {
        match self {
            Self::Ask => "Every action that would leave the sandbox waits for your approval.",
            Self::Never => {
                "Actions inside the sandbox run without asking. Needs a paired device and a finished setup."
            }
        }
    }

    /// Parse a stored or submitted value.
    pub fn parse(value: &str) -> Result<Self, PermissionError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ask" => Ok(Self::Ask),
            "never" => Ok(Self::Never),
            other => Err(PermissionError::UnknownValue {
                field: "approval_policy",
                value: other.to_owned(),
            }),
        }
    }
}

impl fmt::Display for ApprovalPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How much autonomy autopilot has. `L2` and `L3` do not exist in 1.0, so the
/// enum does not have them either: a level that is not implemented cannot be
/// selected by a crafted request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutopilotCeiling {
    /// Watch and report only.
    L0Notify,
    /// Prepare drafts for review.
    L1Draft,
}

impl AutopilotCeiling {
    /// The default: drafts, never sends.
    pub const DEFAULT: Self = Self::L1Draft;

    /// Every option, in display order.
    pub const ALL: [Self; 2] = [Self::L0Notify, Self::L1Draft];

    /// The wire and database spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::L0Notify => "L0 notify",
            Self::L1Draft => "L1 draft",
        }
    }

    /// The label a person reads.
    #[must_use]
    pub fn label(self) -> &'static str {
        self.as_str()
    }

    /// One line of plain English: what changes if you pick this.
    #[must_use]
    pub fn consequence(self) -> &'static str {
        match self {
            Self::L0Notify => "Autopilot only watches and tells you something happened.",
            Self::L1Draft => "Autopilot prepares drafts for you to review. Nothing is sent.",
        }
    }

    /// Parse a stored or submitted value. `L2` and `L3` are refused by name so
    /// the refusal explains itself instead of reading as a typo.
    pub fn parse(value: &str) -> Result<Self, PermissionError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "l0 notify" | "l0" | "l0-notify" | "notify" => Ok(Self::L0Notify),
            "l1 draft" | "l1" | "l1-draft" | "draft" => Ok(Self::L1Draft),
            "l2" | "l3" | "l2 act" | "l3 autonomous" => Err(PermissionError::CeilingAbsent {
                value: value.trim().to_owned(),
            }),
            other => Err(PermissionError::UnknownValue {
                field: "autopilot_ceiling",
                value: other.to_owned(),
            }),
        }
    }
}

impl fmt::Display for AutopilotCeiling {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where counters go. There is no cloud sink in 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Telemetry {
    /// Nothing recorded beyond the local audit rows.
    Off,
    /// Counters in the local database, never uploaded.
    LocalOnly,
}

impl Telemetry {
    /// The default: off.
    pub const DEFAULT: Self = Self::Off;

    /// Every option, in display order.
    pub const ALL: [Self; 2] = [Self::Off, Self::LocalOnly];

    /// The wire and database spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::LocalOnly => "local only",
        }
    }

    /// The label a person reads.
    #[must_use]
    pub fn label(self) -> &'static str {
        self.as_str()
    }

    /// One line of plain English: what changes if you pick this.
    #[must_use]
    pub fn consequence(self) -> &'static str {
        match self {
            Self::Off => "No usage counters are recorded at all.",
            Self::LocalOnly => {
                "Counters are written to this host's database. They are never uploaded anywhere."
            }
        }
    }

    /// Parse a stored or submitted value.
    pub fn parse(value: &str) -> Result<Self, PermissionError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "local only" | "local-only" | "local" => Ok(Self::LocalOnly),
            other => Err(PermissionError::UnknownValue {
                field: "telemetry",
                value: other.to_owned(),
            }),
        }
    }
}

impl fmt::Display for Telemetry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a permission value was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PermissionError {
    /// A value outside the closed set.
    #[error("{field} does not accept that value")]
    UnknownValue {
        /// The form field.
        field: &'static str,
        /// What was submitted. Echoed because a permission label is not a secret.
        value: String,
    },
    /// `full` was submitted through onboarding.
    #[error("onboarding cannot select full sandbox mode: restart eris with --allow-full")]
    FullNotAvailableInOnboarding,
    /// `never` was submitted before a device is paired and setup is finished.
    #[error("the never approval policy needs a paired device and a finished setup")]
    NeverRequiresPairedDevice,
    /// `L2` or `L3` was submitted.
    #[error("{value} is not a level in 1.0: autopilot is L0 notify or L1 draft")]
    CeilingAbsent {
        /// What was submitted.
        value: String,
    },
}

impl PermissionError {
    /// The stable machine-readable code for this refusal.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownValue { .. } => "unknown_value",
            Self::FullNotAvailableInOnboarding => "sandbox_full_not_available",
            Self::NeverRequiresPairedDevice => "approval_never_requires_device",
            Self::CeilingAbsent { .. } => "autopilot_ceiling_absent",
        }
    }

    /// The form field the refusal belongs to.
    #[must_use]
    pub fn field(&self) -> &'static str {
        match self {
            Self::UnknownValue { field, .. } => field,
            Self::FullNotAvailableInOnboarding => "sandbox_mode",
            Self::NeverRequiresPairedDevice => "approval_policy",
            Self::CeilingAbsent { .. } => "autopilot_ceiling",
        }
    }
}

/// One submitted set of permission values, before validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// Sandbox mode.
    pub sandbox_mode: SandboxMode,
    /// Approval policy.
    pub approval_policy: ApprovalPolicy,
    /// Autopilot ceiling.
    pub autopilot_ceiling: AutopilotCeiling,
    /// Telemetry destination.
    pub telemetry: Telemetry,
}

impl Permissions {
    /// The defaults: read-only, ask, L1 draft, telemetry off.
    pub const DEFAULT: Self = Self {
        sandbox_mode: SandboxMode::DEFAULT,
        approval_policy: ApprovalPolicy::DEFAULT,
        autopilot_ceiling: AutopilotCeiling::DEFAULT,
        telemetry: Telemetry::DEFAULT,
    };

    /// Validate one submitted tuple.
    ///
    /// `device_paired` and `ready` are the two facts that decide whether
    /// `never` is offered. Onboarding passes `false` for `ready` because the
    /// policy is chosen before the wizard is finished, which is exactly why
    /// `never` is refused there and allowed from the dashboard.
    pub fn validate(
        sandbox_mode: &str,
        approval_policy: &str,
        autopilot_ceiling: &str,
        telemetry: &str,
        device_paired: bool,
        ready: bool,
    ) -> Result<Self, PermissionError> {
        Self::validate_with(
            sandbox_mode,
            approval_policy,
            autopilot_ceiling,
            telemetry,
            device_paired,
            ready,
            false,
        )
    }

    /// The same validation with one extra fact: whether this process was started
    /// with `--allow-full`.
    ///
    /// Onboarding passes `false` by construction. The dashboard passes the
    /// process flag, which is the only way `full` can ever be stored: it takes a
    /// restart with an explicit flag, not a form post.
    pub fn validate_with(
        sandbox_mode: &str,
        approval_policy: &str,
        autopilot_ceiling: &str,
        telemetry: &str,
        device_paired: bool,
        ready: bool,
        full_allowed: bool,
    ) -> Result<Self, PermissionError> {
        let sandbox_mode = SandboxMode::parse(sandbox_mode)?;
        let approval_policy = ApprovalPolicy::parse(approval_policy)?;
        let autopilot_ceiling = AutopilotCeiling::parse(autopilot_ceiling)?;
        let telemetry = Telemetry::parse(telemetry)?;

        if sandbox_mode == SandboxMode::Full && !full_allowed {
            return Err(PermissionError::FullNotAvailableInOnboarding);
        }
        if approval_policy == ApprovalPolicy::Never && !(device_paired && ready) {
            return Err(PermissionError::NeverRequiresPairedDevice);
        }
        Ok(Self {
            sandbox_mode,
            approval_policy,
            autopilot_ceiling,
            telemetry,
        })
    }
}

/// Which wire strategy a permission row uses in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowControl {
    /// A radio group over a closed set.
    Choice,
    /// A locked row with a reason and no control.
    Locked,
}

/// One row of the permission table, ready to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRow {
    /// Stable field name, used as the form input name.
    pub field: &'static str,
    /// Human name.
    pub name: &'static str,
    /// Current value, as displayed.
    pub value: String,
    /// The consequence string for the current value.
    pub consequence: &'static str,
    /// Choice or locked.
    pub control: RowControl,
    /// Why the row is locked, when it is.
    pub lock_reason: Option<&'static str>,
    /// Selectable values as `(value, label, consequence)`, empty when locked.
    pub options: Vec<(&'static str, &'static str, &'static str)>,
    /// False when the row exists to be read, not changed.
    pub selectable: bool,
}

/// Build the whole table for a state.
///
/// `workspace` is passed in because it lives in the same form; the two locked
/// rows and the network row are constants by design.
#[must_use]
pub fn permission_rows(
    permissions: Permissions,
    workspace: &str,
    device_paired: bool,
    ready: bool,
) -> Vec<PermissionRow> {
    let mut rows = Vec::with_capacity(8);

    rows.push(PermissionRow {
        field: "sandbox_mode",
        name: "Sandbox mode",
        value: permissions.sandbox_mode.label().to_owned(),
        consequence: permissions.sandbox_mode.consequence(),
        control: RowControl::Choice,
        lock_reason: None,
        options: SandboxMode::ONBOARDING
            .iter()
            .map(|mode| (mode.as_str(), mode.label(), mode.consequence()))
            .collect(),
        selectable: true,
    });

    let never_available = device_paired && ready;
    rows.push(PermissionRow {
        field: "approval_policy",
        name: "Approval policy",
        value: permissions.approval_policy.label().to_owned(),
        consequence: permissions.approval_policy.consequence(),
        control: RowControl::Choice,
        lock_reason: if never_available {
            None
        } else {
            Some("Never ask unlocks after a device is paired and setup is finished.")
        },
        options: ApprovalPolicy::ALL
            .iter()
            .filter(|policy| **policy != ApprovalPolicy::Never || never_available)
            .map(|policy| (policy.as_str(), policy.label(), policy.consequence()))
            .collect(),
        selectable: true,
    });

    rows.push(PermissionRow {
        field: "autopilot_ceiling",
        name: "Autopilot ceiling",
        value: permissions.autopilot_ceiling.label().to_owned(),
        consequence: permissions.autopilot_ceiling.consequence(),
        control: RowControl::Choice,
        lock_reason: None,
        options: AutopilotCeiling::ALL
            .iter()
            .map(|level| (level.as_str(), level.label(), level.consequence()))
            .collect(),
        selectable: true,
    });

    rows.push(PermissionRow {
        field: "external_comm",
        name: "External communication",
        value: "draft only".to_owned(),
        consequence: "1.0 never transmits in your voice. Drafts stop at the review screen.",
        control: RowControl::Locked,
        lock_reason: Some("1.0 never transmits in your voice"),
        options: Vec::new(),
        selectable: false,
    });

    rows.push(PermissionRow {
        field: "financial",
        name: "Financial",
        value: "disabled".to_owned(),
        consequence: "No payment, banking, or brokerage action exists in 1.0.",
        control: RowControl::Locked,
        lock_reason: Some("there is no financial action to enable in 1.0"),
        options: Vec::new(),
        selectable: false,
    });

    rows.push(PermissionRow {
        field: "workspace",
        name: "Workspace",
        value: workspace.to_owned(),
        consequence: "The one directory a turn may write to. Everything outside it is out of reach.",
        control: RowControl::Choice,
        lock_reason: None,
        options: Vec::new(),
        selectable: true,
    });

    rows.push(PermissionRow {
        field: "network",
        name: "Network",
        value: "loopback only".to_owned(),
        consequence: "The device listener stays on 127.0.0.1 and is off until it is enabled on the dashboard.",
        control: RowControl::Locked,
        lock_reason: Some("loopback only is the 1.0 rule, not a choice"),
        options: Vec::new(),
        selectable: false,
    });

    rows.push(PermissionRow {
        field: "telemetry",
        name: "Telemetry",
        value: permissions.telemetry.label().to_owned(),
        consequence: permissions.telemetry.consequence(),
        control: RowControl::Choice,
        lock_reason: None,
        options: Telemetry::ALL
            .iter()
            .map(|sink| (sink.as_str(), sink.label(), sink.consequence()))
            .collect(),
        selectable: true,
    });

    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_documented_ones() {
        let defaults = Permissions::DEFAULT;
        assert_eq!(defaults.sandbox_mode, SandboxMode::ReadOnly);
        assert_eq!(defaults.approval_policy, ApprovalPolicy::Ask);
        assert_eq!(defaults.autopilot_ceiling, AutopilotCeiling::L1Draft);
        assert_eq!(defaults.telemetry, Telemetry::Off);
        assert_eq!(defaults.sandbox_mode.as_str(), "read-only");
        assert_eq!(defaults.approval_policy.as_str(), "ask");
        assert_eq!(defaults.autopilot_ceiling.as_str(), "L1 draft");
        assert_eq!(defaults.telemetry.as_str(), "off");
    }

    #[test]
    fn onboarding_refuses_full_sandbox_mode() {
        let refused = Permissions::validate("full", "ask", "L1 draft", "off", false, false)
            .expect_err("full must not be selectable in onboarding");
        assert_eq!(refused.code(), "sandbox_full_not_available");
        assert_eq!(refused.field(), "sandbox_mode");
        assert!(
            !SandboxMode::ONBOARDING.contains(&SandboxMode::Full),
            "the offered set must not contain full"
        );
    }

    #[test]
    fn never_is_refused_until_a_device_is_paired_and_setup_is_finished() {
        for (paired, ready) in [(false, false), (true, false), (false, true)] {
            let refused =
                Permissions::validate("read-only", "never", "L1 draft", "off", paired, ready)
                    .expect_err("never needs both facts");
            assert_eq!(refused.code(), "approval_never_requires_device");
        }
        let allowed = Permissions::validate("read-only", "never", "L1 draft", "off", true, true)
            .expect("a paired device and a ready daemon allow never");
        assert_eq!(allowed.approval_policy, ApprovalPolicy::Never);
    }

    #[test]
    fn l2_and_l3_are_refused_by_name() {
        for level in ["L2", "L3", "l2 act"] {
            let refused = Permissions::validate("read-only", "ask", level, "off", false, false)
                .expect_err("no such level");
            assert_eq!(refused.code(), "autopilot_ceiling_absent");
        }
    }

    #[test]
    fn an_unknown_value_names_its_field() {
        let refused = Permissions::validate("yolo", "ask", "L1 draft", "off", false, false)
            .expect_err("refused");
        assert_eq!(refused.code(), "unknown_value");
        assert_eq!(refused.field(), "sandbox_mode");
        assert!(refused.to_string().contains("sandbox_mode"), "{refused}");
    }

    #[test]
    fn the_locked_rows_are_locked_and_carry_a_reason() {
        let rows = permission_rows(Permissions::DEFAULT, "/tmp/ws", false, false);
        let by_field = |name: &str| {
            rows.iter()
                .find(|row| row.field == name)
                .unwrap_or_else(|| panic!("no row for {name}"))
        };
        for field in ["external_comm", "financial", "network"] {
            let row = by_field(field);
            assert_eq!(row.control, RowControl::Locked, "{field}");
            assert!(!row.selectable, "{field}");
            assert!(row.lock_reason.is_some(), "{field} needs a reason");
        }
        assert_eq!(by_field("external_comm").value, "draft only");
        assert_eq!(by_field("financial").value, "disabled");
        assert_eq!(by_field("network").value, "loopback only");
        assert_eq!(
            by_field("external_comm").lock_reason,
            Some("1.0 never transmits in your voice")
        );
    }

    #[test]
    fn never_is_not_offered_before_a_device_is_paired() {
        let rows = permission_rows(Permissions::DEFAULT, "/tmp/ws", false, false);
        let approval = rows
            .iter()
            .find(|row| row.field == "approval_policy")
            .expect("approval row");
        assert!(
            approval
                .options
                .iter()
                .all(|(value, _, _)| *value != "never"),
            "the never option must be absent while it is refused"
        );
        assert!(approval.lock_reason.is_some());

        let ready_rows = permission_rows(Permissions::DEFAULT, "/tmp/ws", true, true);
        let ready_approval = ready_rows
            .iter()
            .find(|row| row.field == "approval_policy")
            .expect("approval row");
        assert!(
            ready_approval
                .options
                .iter()
                .any(|(value, _, _)| *value == "never")
        );
    }

    #[test]
    fn every_row_carries_a_one_line_consequence() {
        for row in permission_rows(Permissions::DEFAULT, "/tmp/ws", false, false) {
            assert!(
                !row.consequence.is_empty(),
                "{} has no consequence",
                row.field
            );
            assert!(
                !row.consequence.contains('\n'),
                "{} consequence is not one line",
                row.field
            );
            for banned in ['\u{2014}', '\u{2013}', '!'] {
                assert!(
                    !row.consequence.contains(banned),
                    "{} consequence carries a banned character {banned:?}",
                    row.field
                );
            }
        }
    }

    #[test]
    fn every_option_label_round_trips_through_parse() {
        for mode in SandboxMode::ALL {
            assert_eq!(SandboxMode::parse(mode.as_str()).expect("parses"), mode);
        }
        for policy in ApprovalPolicy::ALL {
            assert_eq!(
                ApprovalPolicy::parse(policy.as_str()).expect("parses"),
                policy
            );
        }
        for level in AutopilotCeiling::ALL {
            assert_eq!(
                AutopilotCeiling::parse(level.as_str()).expect("parses"),
                level
            );
        }
        for sink in Telemetry::ALL {
            assert_eq!(Telemetry::parse(sink.as_str()).expect("parses"), sink);
        }
    }
}
