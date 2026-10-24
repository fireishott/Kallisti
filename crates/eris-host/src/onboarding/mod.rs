//! The onboarding engine: the first-run state machine, the server-side wizard,
//! and the permission values the rest of 1.0 reads.
//!
//! Two rules shape this module.
//!
//! **State is a row, not a session.** Every wizard step leaves a row in
//! `settings` with a `completed_at`, so a browser refresh, a closed tab, or a
//! restarted daemon all land back on the same step. Nothing about progress lives
//! in memory or in a cookie.
//!
//! **`setup` is the absence of a credential.** `serve` reads the state from
//! `app_state`, but a database that says `ready` while `admin_credential` is
//! empty is not trustworthy: [`SetupEngine::reconcile_first_boot`] forces it back
//! to `setup`, which is what makes `eris setup --reset` a real recovery path
//! rather than a flag.
//!
//! Nothing here prints or stores a passphrase value. The one plaintext copy is
//! `~/.config/kallisti/first-run.txt` at mode `0600`, written once, and the
//! verifier in `settings` is what a claim is checked against.

pub mod passphrase;
pub mod permissions;
pub mod ratelimit;
pub mod session;
pub mod wordlist;
pub mod workspace;

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};

use crate::auth::AuthStore;
use crate::config::Paths;
use crate::db::{Db, DbError, SetupState};

pub use passphrase::{
    FIRST_RUN_FILE_NAME, FIRST_RUN_MODE, PassphraseError, WORD_COUNT, first_run_block,
    pending_block,
};
pub use permissions::{
    ApprovalPolicy, AutopilotCeiling, PermissionError, PermissionRow, Permissions, RowControl,
    SandboxMode, Telemetry, permission_rows,
};
pub use ratelimit::{CLAIM_LIMIT, CLAIM_WINDOW, ClaimLimiter};
pub use session::{AdminSessions, SessionError};
pub use workspace::{PreparedWorkspace, REFUSED_ROOTS, WorkspaceError, default_workspace};

/// `settings` key holding the Argon2id verifier of the setup passphrase.
pub const SETTING_PASSPHRASE_HASH: &str = "setup.passphrase_hash";

/// `settings` key holding the generated-at timestamp.
pub const SETTING_PASSPHRASE_CREATED: &str = "setup.passphrase_created_at";

/// `settings` key: the provider base URL.
pub const SETTING_PROVIDER_BASE_URL: &str = "provider.base_url";

/// `settings` key: the provider key reference (`env:NAME`, never a value).
pub const SETTING_PROVIDER_KEY_REF: &str = "provider.api_key_ref";

/// `settings` key: the JSON array of model ids the probe listed.
/// `settings` key: whether the dashboard may select full host sandbox mode.
///
/// The command line flag is still the override: `--allow-full` turns this on for
/// the life of the process. This row is the operator's own switch, written from
/// the dashboard behind a typed confirmation and audited, so enabling it does not
/// need a shell.
pub const SETTING_ALLOW_FULL: &str = "permissions.allow_full";

pub const SETTING_PROVIDER_MODELS: &str = "provider.models";

/// `settings` key: the context window each probed model declared, as a JSON
/// object of id to token count. Written by the probe, read by the chat surface so
/// a context percentage is a number the provider published rather than a guess.
pub const SETTING_PROVIDER_CONTEXTS: &str = "provider.context_lengths";

/// `settings` key: when the last successful probe happened.
pub const SETTING_PROVIDER_PROBED_AT: &str = "provider.probed_at";

/// `settings` key: the default model id.
pub const SETTING_MODEL: &str = "provider.model";

/// `settings` key: sandbox mode.
pub const SETTING_SANDBOX_MODE: &str = "permissions.sandbox_mode";

/// `settings` key: approval policy.
pub const SETTING_APPROVAL_POLICY: &str = "permissions.approval_policy";

/// `settings` key: autopilot ceiling.
pub const SETTING_AUTOPILOT_CEILING: &str = "permissions.autopilot_ceiling";

/// `settings` key: telemetry destination.
pub const SETTING_TELEMETRY: &str = "permissions.telemetry";

/// `settings` key: whether the operator asked for the device listener. P7 only
/// records the intent; P8 is the phase that may bind the port.
pub const SETTING_DEVICE_LISTENER: &str = "permissions.device_listener_enabled";

/// `settings` key: the sandbox workspace path.
pub const SETTING_WORKSPACE: &str = "workspace.path";

/// The one definition of a paired device, used by the count and the dashboard.
///
/// A device counts when it is not revoked and either a live token exists for it
/// (P6 `auth_tokens`) or one of its pairing codes was redeemed (`consumed_at`).
/// A registered row with a code that nobody redeemed does not count.
pub const PAIRED_DEVICE_COUNT_SQL: &str = "
SELECT COUNT(*)
FROM devices d
WHERE d.revoked_at IS NULL
  AND (
    EXISTS (
      SELECT 1 FROM auth_tokens t
      WHERE t.device_id = d.id AND t.revoked_at IS NULL AND t.rotated_at IS NULL
    )
    OR EXISTS (
      SELECT 1 FROM pairing_codes p
      WHERE p.device_id = d.id AND p.consumed_at IS NOT NULL
    )
  )";

/// Prefix every per-step row uses: `setup.step.<id>`.
pub const STEP_KEY_PREFIX: &str = "setup.step.";

/// The wizard, in order. `device` is the one optional step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Step {
    /// Verify the six-word passphrase and open an admin session.
    Claim,
    /// Validate an endpoint with a real probe and store it.
    Provider,
    /// Pick the default model from the probe's list.
    Models,
    /// Sandbox mode, approval policy, autopilot ceiling, telemetry.
    Permissions,
    /// Workspace path, created at 0755 when absent.
    Workspace,
    /// Mint one pairing code. Optional.
    Device,
    /// Lock the wizard and move the daemon to `ready`.
    Finish,
}

impl Step {
    /// Every step, in order.
    pub const ALL: [Self; 7] = [
        Self::Claim,
        Self::Provider,
        Self::Models,
        Self::Permissions,
        Self::Workspace,
        Self::Device,
        Self::Finish,
    ];

    /// The steps that must be complete before `finish` is accepted.
    pub const REQUIRED: [Self; 6] = [
        Self::Claim,
        Self::Provider,
        Self::Models,
        Self::Permissions,
        Self::Workspace,
        Self::Finish,
    ];

    /// The stable id, which is the URL segment and the settings suffix.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Provider => "provider",
            Self::Models => "models",
            Self::Permissions => "permissions",
            Self::Workspace => "workspace",
            Self::Device => "device",
            Self::Finish => "finish",
        }
    }

    /// The heading a person reads.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Claim => "Claim this daemon",
            Self::Provider => "Provider",
            Self::Models => "Model",
            Self::Permissions => "Permissions",
            Self::Workspace => "Workspace",
            Self::Device => "Device",
            Self::Finish => "Finish",
        }
    }

    /// One line of plain English about the step.
    #[must_use]
    pub fn summary(self) -> &'static str {
        match self {
            Self::Claim => "Enter the six-word passphrase printed on the first boot.",
            Self::Provider => {
                "Point at an OpenAI-compatible endpoint. The key is an environment reference, never a value."
            }
            Self::Models => "Choose the model a turn uses by default.",
            Self::Permissions => "Pick what a turn may touch, and whether it has to ask first.",
            Self::Workspace => "Choose the one directory a turn may write to.",
            Self::Device => "Optional. Mint a pairing code now, or pair later from the dashboard.",
            Self::Finish => "Lock the wizard. The daemon moves to ready.",
        }
    }

    /// True for the one step that can be skipped.
    #[must_use]
    pub fn is_optional(self) -> bool {
        matches!(self, Self::Device)
    }

    /// The `settings` key this step writes.
    #[must_use]
    pub fn key(self) -> String {
        format!("{STEP_KEY_PREFIX}{}", self.as_str())
    }
}

/// Everything the onboarding engine can refuse.
#[derive(Debug, thiserror::Error)]
pub enum OnboardingError {
    /// SQLite refused.
    #[error("database error: {0}")]
    Db(#[from] DbError),
    /// The auth primitives refused.
    #[error(transparent)]
    Auth(#[from] crate::auth::AuthError),
    /// A session operation refused.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// The passphrase file could not be written.
    #[error(transparent)]
    Passphrase(#[from] PassphraseError),
    /// The workspace was refused.
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    /// A permission value was refused.
    #[error(transparent)]
    Permission(#[from] PermissionError),
    /// The passphrase did not verify.
    #[error("the setup passphrase was rejected")]
    PassphraseRejected,
    /// No passphrase verifier exists, which means the daemon never booted in
    /// setup state on this state directory.
    #[error("no setup passphrase is stored: this daemon has already completed onboarding")]
    NoPassphrase,
    /// A step was attempted before its prerequisite.
    #[error("step {step} cannot run before {needed}")]
    StepOutOfOrder {
        /// The step that was attempted.
        step: &'static str,
        /// The step that must come first.
        needed: &'static str,
    },
    /// `finish` was attempted with a required step still open.
    #[error("setup cannot finish while {step} is still open")]
    StepIncomplete {
        /// The first incomplete required step.
        step: &'static str,
    },
    /// A wizard POST arrived after onboarding was already finished.
    #[error("onboarding is finished; the wizard is locked")]
    WizardLocked,
    /// The endpoint was blank or not http(s).
    #[error("the provider endpoint must be an http or https URL")]
    ProviderEndpoint,
    /// The key reference was not `env:NAME`.
    #[error("the key reference must be env:NAME, never a key value")]
    ProviderKeyRef,
    /// The chosen model was not in the probe's list.
    #[error("the model is not in the list the provider returned")]
    ModelNotInProbe,
    /// The model field was blank.
    #[error("a model is required")]
    ModelRequired,
    /// `eris setup --reset` needs the admin passphrase.
    #[error("the existing admin passphrase is required for --reset")]
    AdminPassphraseRequired,
    /// The admin passphrase did not verify.
    #[error("the existing admin passphrase was rejected")]
    AdminPassphraseRejected,
}

impl OnboardingError {
    /// The stable machine-readable code for this refusal.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Db(_) => "db_error",
            Self::Auth(error) => error.code(),
            Self::Session(error) => error.code(),
            Self::Passphrase(_) => "first_run_file_failed",
            Self::Workspace(error) => error.code(),
            Self::Permission(error) => error.code(),
            Self::PassphraseRejected => "passphrase_rejected",
            Self::NoPassphrase => "no_setup_passphrase",
            Self::StepOutOfOrder { .. } => "step_out_of_order",
            Self::StepIncomplete { .. } => "step_incomplete",
            Self::WizardLocked => "wizard_locked",
            Self::ProviderEndpoint => "provider_endpoint_invalid",
            Self::ProviderKeyRef => "provider_key_ref_invalid",
            Self::ModelNotInProbe => "model_not_in_probe",
            Self::ModelRequired => "model_required",
            Self::AdminPassphraseRequired => "admin_passphrase_required",
            Self::AdminPassphraseRejected => "admin_passphrase_rejected",
        }
    }

    /// The form field the refusal belongs to, when it has one.
    #[must_use]
    pub fn field(&self) -> Option<&'static str> {
        match self {
            Self::Workspace(error) => Some(match error {
                WorkspaceError::Empty | WorkspaceError::NotAbsolute { .. } => "workspace_path",
                WorkspaceError::ParentTraversal => "workspace_path",
                WorkspaceError::RefusedOverlap { .. } => "workspace_path",
                WorkspaceError::NotADirectory { .. } => "workspace_path",
                WorkspaceError::NoHome => "workspace_path",
                WorkspaceError::Io { .. } => "workspace_path",
                WorkspaceError::WorldWritable { .. } => "workspace_path",
            }),
            Self::Permission(error) => Some(error.field()),
            Self::PassphraseRejected => Some("passphrase"),
            Self::NoPassphrase => Some("passphrase"),
            Self::ProviderEndpoint => Some("base_url"),
            Self::ProviderKeyRef => Some("api_key_ref"),
            Self::ModelNotInProbe | Self::ModelRequired => Some("model"),
            Self::AdminPassphraseRequired | Self::AdminPassphraseRejected => Some("passphrase"),
            _ => None,
        }
    }
}

/// What a first boot produced.
#[derive(Debug, Clone)]
pub struct FirstRun {
    /// The passphrase, present only on the boot that generated it.
    pub generated: Option<String>,
    /// Where the file lives.
    pub first_run_path: PathBuf,
    /// The state the daemon is in.
    pub state: SetupState,
}

/// One wizard step, as the JSON state endpoint reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepStatus {
    /// The step.
    pub step: Step,
    /// `completed_at`, when it is complete.
    pub completed_at: Option<String>,
    /// True for the one optional step.
    pub optional: bool,
}

/// The whole wizard, as the JSON state endpoint reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wizard {
    /// The daemon's setup state.
    pub state: SetupState,
    /// Every step in order.
    pub steps: Vec<StepStatus>,
    /// The first step that still needs doing, or `None` when finished.
    pub next_step: Option<Step>,
}

impl Wizard {
    /// True when every required step is complete.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.steps
            .iter()
            .filter(|status| !status.optional)
            .all(|status| status.completed_at.is_some())
    }

    /// Whether one step is complete.
    #[must_use]
    pub fn completed(&self, step: Step) -> bool {
        self.steps
            .iter()
            .any(|status| status.step == step && status.completed_at.is_some())
    }
}

/// The onboarding engine: settings rows plus the state machine.
#[derive(Clone)]
pub struct SetupEngine {
    db: Db,
    paths: Paths,
}

impl SetupEngine {
    /// Wrap the state database and the paths the daemon resolved.
    #[must_use]
    pub fn new(db: Db, paths: Paths) -> Self {
        Self { db, paths }
    }

    /// The database underneath.
    #[must_use]
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The paths under `$HOME` (or the scratch root an instance was given).
    #[must_use]
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// Where the first-run file lives.
    #[must_use]
    pub fn first_run_path(&self) -> PathBuf {
        self.paths.config_dir.join(FIRST_RUN_FILE_NAME)
    }

    /// The auth store over the same database.
    #[must_use]
    pub fn auth(&self) -> AuthStore {
        AuthStore::new(self.db.clone())
    }

    /// The admin session store over the same database.
    #[must_use]
    pub fn sessions(&self) -> AdminSessions {
        AdminSessions::new(self.db.clone())
    }

    /// The setup state, as `/healthz` reports it.
    pub fn state(&self) -> Result<SetupState, OnboardingError> {
        Ok(self.db.setup_state()?)
    }

    /// Force `setup` when no admin credential exists.
    ///
    /// `app_state` is the state the operator sees, but the credential is the
    /// fact that makes it true. A `ready` row with no credential means a database
    /// was copied, restored, or hand-edited, and the safe answer is the wizard.
    pub fn reconcile_first_boot(&self) -> Result<SetupState, OnboardingError> {
        let has_credential = self.auth().admin_password_hash()?.is_some();
        if !has_credential && self.state()? == SetupState::Ready {
            self.db.set_setup_state(SetupState::Setup)?;
            self.auth().audit(
                "loopback",
                "setup.reopened",
                Some("app_state said ready but no admin credential exists"),
            )?;
        }
        self.state()
    }

    /// Generate the six-word passphrase exactly once, or report that it exists.
    ///
    /// The file is written before the verifier is stored, so a crash between the
    /// two leaves a readable file and an unclaimed wizard rather than a verifier
    /// nobody can satisfy.
    pub fn ensure_first_run(&self) -> Result<FirstRun, OnboardingError> {
        let state = self.reconcile_first_boot()?;
        let path = self.first_run_path();
        if state == SetupState::Ready {
            return Ok(FirstRun {
                generated: None,
                first_run_path: path,
                state,
            });
        }
        if self.setting(SETTING_PASSPHRASE_HASH)?.is_some() {
            return Ok(FirstRun {
                generated: None,
                first_run_path: path,
                state,
            });
        }

        let value = passphrase::generate()?;
        passphrase::write_first_run(&path, &value)?;
        let verifier = crate::auth::hash_secret(value.as_bytes())?;
        self.set_setting(SETTING_PASSPHRASE_HASH, &verifier)?;
        self.set_setting(
            SETTING_PASSPHRASE_CREATED,
            &crate::auth::format_timestamp(crate::auth::now()),
        )?;
        self.auth().audit(
            "loopback",
            "setup.passphrase_generated",
            Some(&path.display().to_string()),
        )?;
        Ok(FirstRun {
            generated: Some(value),
            first_run_path: path,
            state,
        })
    }

    /// Check a claimed passphrase against the stored verifier.
    pub fn verify_passphrase(&self, presented: &str) -> Result<(), OnboardingError> {
        let stored = self
            .setting(SETTING_PASSPHRASE_HASH)?
            .ok_or(OnboardingError::NoPassphrase)?;
        let normalized = passphrase::normalize(presented);
        let matches = crate::auth::verify_secret(normalized.as_bytes(), &stored)?;
        if matches {
            Ok(())
        } else {
            self.auth()
                .audit("loopback", "setup.passphrase_rejected", None)?;
            Err(OnboardingError::PassphraseRejected)
        }
    }

    /// Accept a claim: verify the passphrase, store it as the admin credential
    /// exactly once, and mark the step.
    ///
    /// The credential is the passphrase itself. That is what makes `eris setup
    /// --reset` able to ask for "the existing admin passphrase" a week later, and
    /// it is why the file the operator was told to keep matters. Only the
    /// Argon2id verifier is stored, exactly as `auth::store` documents.
    pub fn accept_claim(&self, presented: &str) -> Result<(), OnboardingError> {
        self.verify_passphrase(presented)?;
        if self.auth().admin_password_hash()?.is_none() {
            self.auth()
                .set_admin_password(&passphrase::normalize(presented))
                .map_err(OnboardingError::Auth)?;
        }
        self.complete_step(Step::Claim)?;
        self.auth().audit("loopback", "setup.claim", None)?;
        Ok(())
    }

    /// Read one `settings` value.
    pub fn setting(&self, key: &str) -> Result<Option<String>, OnboardingError> {
        let value = self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()
        })?;
        Ok(value)
    }

    /// Write one `settings` value, creating or replacing the row.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), OnboardingError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value, \
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
                params![key, value],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Delete one `settings` row.
    pub fn clear_setting(&self, key: &str) -> Result<(), OnboardingError> {
        self.db.with_conn(|conn| {
            conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
            Ok(())
        })?;
        Ok(())
    }

    /// When one step completed, if it did.
    pub fn step_completed_at(&self, step: Step) -> Result<Option<String>, OnboardingError> {
        let Some(json) = self.setting(&step.key())? else {
            return Ok(None);
        };
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
        Ok(parsed
            .get("completed_at")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned))
    }

    /// Mark one step complete. Idempotent: a second call keeps the first stamp.
    pub fn complete_step(&self, step: Step) -> Result<(), OnboardingError> {
        if self.step_completed_at(step)?.is_some() {
            return Ok(());
        }
        let payload = serde_json::json!({
            "completed_at": crate::auth::format_timestamp(crate::auth::now()),
        });
        self.set_setting(&step.key(), &payload.to_string())?;
        Ok(())
    }

    /// The whole wizard, read from `settings`.
    pub fn wizard(&self) -> Result<Wizard, OnboardingError> {
        let state = self.state()?;
        let mut steps = Vec::with_capacity(Step::ALL.len());
        let mut next_step = None;
        for step in Step::ALL {
            let completed_at = self.step_completed_at(step)?;
            if completed_at.is_none() && next_step.is_none() && !step.is_optional() {
                next_step = Some(step);
            }
            steps.push(StepStatus {
                step,
                completed_at,
                optional: step.is_optional(),
            });
        }
        if next_step.is_none() && state == SetupState::Setup {
            // Every required step is done but the daemon has not been finished
            // yet: the only thing left is `finish`.
            next_step = Some(Step::Finish);
        }
        Ok(Wizard {
            state,
            steps,
            next_step,
        })
    }

    /// Refuse a step that arrives before its prerequisite.
    pub fn require_step(&self, step: Step) -> Result<(), OnboardingError> {
        const ORDER: [Step; 6] = [
            Step::Claim,
            Step::Provider,
            Step::Models,
            Step::Permissions,
            Step::Workspace,
            Step::Finish,
        ];
        let Some(index) = ORDER.iter().position(|candidate| *candidate == step) else {
            // The optional device step sits between workspace and finish.
            if step == Step::Device {
                if self.step_completed_at(Step::Workspace)?.is_none() {
                    return Err(OnboardingError::StepOutOfOrder {
                        step: step.as_str(),
                        needed: Step::Workspace.as_str(),
                    });
                }
                return Ok(());
            }
            return Ok(());
        };
        if index == 0 {
            return Ok(());
        }
        let previous = ORDER[index - 1];
        if self.step_completed_at(previous)?.is_none() {
            return Err(OnboardingError::StepOutOfOrder {
                step: step.as_str(),
                needed: previous.as_str(),
            });
        }
        Ok(())
    }

    /// Refuse any wizard write once onboarding is finished.
    pub fn ensure_wizard_open(&self) -> Result<(), OnboardingError> {
        if self.reconcile_first_boot()? == SetupState::Ready {
            return Err(OnboardingError::WizardLocked);
        }
        Ok(())
    }

    /// Store the provider the probe validated.
    pub fn set_provider(
        &self,
        base_url: &str,
        key_ref: &str,
        models: &[String],
    ) -> Result<(), OnboardingError> {
        let trimmed = base_url.trim().trim_end_matches('/');
        if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
            return Err(OnboardingError::ProviderEndpoint);
        }
        // A reference, not a value. `env:` is the only scheme 1.0 accepts, and
        // the parser is the same one the config file uses.
        if core_domain::SecretRef::try_from(key_ref.trim()).is_err() {
            return Err(OnboardingError::ProviderKeyRef);
        }
        let catalog = serde_json::to_string(models).unwrap_or_else(|_| "[]".to_owned());
        self.set_setting(SETTING_PROVIDER_BASE_URL, trimmed)?;
        self.set_setting(SETTING_PROVIDER_KEY_REF, key_ref.trim())?;
        self.set_setting(SETTING_PROVIDER_MODELS, &catalog)?;
        self.set_setting(
            SETTING_PROVIDER_PROBED_AT,
            &crate::auth::format_timestamp(crate::auth::now()),
        )?;
        self.complete_step(Step::Provider)?;
        self.auth().audit(
            "loopback",
            "setup.provider",
            Some(&format!("{trimmed} models={}", models.len())),
        )?;
        Ok(())
    }

    /// The provider base URL, when one was stored.
    pub fn provider_base_url(&self) -> Result<Option<String>, OnboardingError> {
        self.setting(SETTING_PROVIDER_BASE_URL)
    }

    /// The provider key reference, when one was stored. Never a value.
    pub fn provider_key_ref(&self) -> Result<Option<String>, OnboardingError> {
        self.setting(SETTING_PROVIDER_KEY_REF)
    }

    /// One provider's own catalogue, keyed by provider id.
    ///
    /// Before this existed the daemon had a single `provider.models` row that
    /// every consumer read, which was true while there was one provider and a
    /// lie the moment the registry could hold two: a model published by the
    /// second provider was refused as "not in the probe". The keyed row is
    /// written by the probe that ran against that provider, and the legacy row
    /// is the seed for the provider it was written for.
    #[must_use]
    pub fn provider_catalog_key(provider_id: &str) -> String {
        format!("provider.catalog.{provider_id}")
    }

    /// One provider's declared context windows, keyed by provider id.
    #[must_use]
    pub fn provider_contexts_key(provider_id: &str) -> String {
        format!("provider.contexts.{provider_id}")
    }

    /// One provider's per-model request shape, keyed by provider id.
    ///
    /// A catalogue lists what a gateway serves and says nothing about how to ask
    /// for it, and one gateway here serves two shapes at once: its Claude models
    /// answer only on `/messages` and everything else on `/chat/completions`.
    /// The probe measures that per model, so the answer is stored per model
    /// rather than inferred from the provider it came from.
    #[must_use]
    pub fn provider_dialects_key(provider_id: &str) -> String {
        format!("provider.dialects.{provider_id}")
    }

    /// The models one provider listed but refused to serve, keyed by provider id.
    ///
    /// Listing is not serving: a probe found seven models this gateway names and
    /// will not answer for. Storing that is what lets a picker mark them instead
    /// of offering a model that 404s the first turn.
    #[must_use]
    pub fn provider_unreachable_key(provider_id: &str) -> String {
        format!("provider.unreachable.{provider_id}")
    }

    /// Seed the keyed rows from the legacy rows, once.
    ///
    /// Every install that predates this carries a single probe under
    /// `provider.models`, and that probe was taken against whichever endpoint
    /// the three legacy rows name. So the seed is exact rather than a guess: it
    /// copies the legacy catalogue to the provider that endpoint belongs to.
    ///
    /// Returns the provider ids it wrote, so a caller can report what happened
    /// rather than assume it.
    pub fn migrate_provider_catalog(&self) -> Result<Vec<String>, OnboardingError> {
        let mut written: Vec<String> = Vec::new();
        let Some(models) = self.setting(SETTING_PROVIDER_MODELS)? else {
            return Ok(written);
        };
        if models.trim().is_empty() {
            return Ok(written);
        }
        let owner = self
            .setting(SETTING_PROVIDER_KEY_REF)?
            .map(|key_ref| crate::providers::legacy_provider_id(&self.providers(), &key_ref))
            .unwrap_or_else(|| crate::providers::DEFAULT_ID.to_owned());
        let key = Self::provider_catalog_key(&owner);
        if self.setting(&key)?.is_none() {
            self.set_setting(&key, &models)?;
            written.push(owner.clone());
        }
        if let Some(contexts) = self.setting(SETTING_PROVIDER_CONTEXTS)?
            && self
                .setting(&Self::provider_contexts_key(&owner))?
                .is_none()
        {
            self.set_setting(&Self::provider_contexts_key(&owner), &contexts)?;
        }
        Ok(written)
    }

    /// The registry, as this engine reads it. A registry that does not parse is
    /// an empty list here: the caller that needs the legacy fallback asks
    /// `chat::provider_registry`, which owns that rule.
    fn providers(&self) -> Vec<crate::providers::Provider> {
        self.setting(crate::providers::SETTING_REGISTRY)
            .ok()
            .flatten()
            .and_then(|raw| crate::providers::parse(&raw).ok())
            .unwrap_or_default()
    }

    /// One provider's catalogue, falling back to the legacy row for the
    /// provider the legacy probe was taken against.
    ///
    /// An empty result is a real answer: it means no probe has published a list
    /// for that provider, which is a different state from a probe that published
    /// an empty one, and callers say so.
    pub fn provider_catalog(&self, provider_id: &str) -> Result<Vec<String>, OnboardingError> {
        if let Some(json) = self.setting(&Self::provider_catalog_key(provider_id))? {
            return Ok(serde_json::from_str(&json).unwrap_or_default());
        }
        if self.legacy_probe_owner()?.as_deref() == Some(provider_id) {
            return self.provider_models();
        }
        Ok(Vec::new())
    }

    /// One provider's context windows, on the same fallback rule.
    pub fn provider_contexts_for(
        &self,
        provider_id: &str,
    ) -> Result<std::collections::BTreeMap<String, u64>, OnboardingError> {
        if let Some(json) = self.setting(&Self::provider_contexts_key(provider_id))? {
            return Ok(serde_json::from_str(&json).unwrap_or_default());
        }
        if self.legacy_probe_owner()?.as_deref() == Some(provider_id) {
            return self.provider_contexts();
        }
        Ok(std::collections::BTreeMap::new())
    }

    /// The catalogue of the primary provider.
    ///
    /// This is what a model pin with no provider of its own applies to:
    /// `provider.model` names the model a turn runs on, and a turn resolves its
    /// endpoint through the primary. So the check a pin gets is the primary's
    /// own catalogue. An install with one provider reads exactly what it always
    /// read, because the single entry is the primary and the keyed row falls
    /// back to the legacy one it was seeded from.
    pub fn primary_provider_catalog(&self) -> Result<Vec<String>, OnboardingError> {
        let owner = self.primary_provider_id()?;
        let catalog = self.provider_catalog(&owner)?;
        if !catalog.is_empty() || self.legacy_probe_owner()?.is_some() {
            return Ok(catalog);
        }
        // No key reference is stored, so nothing on this daemon says which
        // provider the legacy probe belonged to - which is a shape that can only
        // hold one provider. The legacy rows are read rather than leaving a pin
        // that looks unpublished on an install that has published one.
        self.provider_models()
    }

    /// The context windows of the primary provider, on the same rule.
    pub fn primary_provider_contexts(
        &self,
    ) -> Result<std::collections::BTreeMap<String, u64>, OnboardingError> {
        let owner = self.primary_provider_id()?;
        let contexts = self.provider_contexts_for(&owner)?;
        if !contexts.is_empty() || self.legacy_probe_owner()?.is_some() {
            return Ok(contexts);
        }
        self.provider_contexts()
    }

    /// The id of the primary provider, resolved the way a turn resolves it.
    ///
    /// A registry that does not parse, or a primary id that names no entry,
    /// yields `default` - the same fallback [`crate::providers::resolve`] applies,
    /// so the settings page and a turn never disagree about which provider is in
    /// force.
    fn primary_provider_id(&self) -> Result<String, OnboardingError> {
        let registry = self.providers();
        let stored = self
            .setting(crate::providers::SETTING_PRIMARY)?
            .unwrap_or_default();
        Ok(crate::providers::resolve(&registry, &stored)
            .map(|provider| provider.id.clone())
            .unwrap_or_else(|| crate::providers::DEFAULT_ID.to_owned()))
    }

    /// The provider a probe writes for.
    ///
    /// The endpoint a probe just called is named by the key reference it used,
    /// so that reference is what identifies whose catalogue this is. A registry
    /// that matches nothing yields `default`, which is the id the legacy rows
    /// themselves belong to.
    pub fn probe_owner(&self) -> Result<String, OnboardingError> {
        Ok(self
            .legacy_probe_owner()?
            .unwrap_or_else(|| crate::providers::DEFAULT_ID.to_owned()))
    }

    /// Every named provider's catalogue, for a client that shows more than one.
    ///
    /// Keyed by provider id, in the order asked for. A provider with no probe of
    /// its own is present with an empty list rather than absent: "this provider
    /// has published nothing" is an answer, and a missing key would make a
    /// client guess at the difference.
    pub fn catalog_map(
        &self,
        provider_ids: &[String],
    ) -> Result<std::collections::BTreeMap<String, Vec<String>>, OnboardingError> {
        let mut map = std::collections::BTreeMap::new();
        for id in provider_ids {
            map.insert(id.clone(), self.provider_catalog(id)?);
        }
        Ok(map)
    }

    /// Which provider the legacy `provider.models` row belongs to.
    fn legacy_probe_owner(&self) -> Result<Option<String>, OnboardingError> {
        let Some(key_ref) = self.setting(SETTING_PROVIDER_KEY_REF)? else {
            return Ok(None);
        };
        Ok(Some(crate::providers::legacy_provider_id(
            &self.providers(),
            &key_ref,
        )))
    }

    /// Record one provider's catalogue and context windows.
    ///
    /// The legacy rows are written too when this is the provider the legacy rows
    /// already described, so an older client reading `provider.models` through
    /// the device API keeps seeing the endpoint it is actually talking to. For
    /// any other provider the legacy rows are left alone: clobbering them would
    /// make a second provider's catalogue masquerade as the first one's.
    pub fn set_provider_catalog(
        &self,
        provider_id: &str,
        models: &[String],
        contexts: &std::collections::BTreeMap<String, u64>,
    ) -> Result<(), OnboardingError> {
        let catalog = serde_json::to_string(models).unwrap_or_else(|_| "[]".to_owned());
        self.set_setting(&Self::provider_catalog_key(provider_id), &catalog)?;
        let windows = serde_json::to_string(contexts).unwrap_or_else(|_| "{}".to_owned());
        self.set_setting(&Self::provider_contexts_key(provider_id), &windows)?;
        self.set_setting(
            SETTING_PROVIDER_PROBED_AT,
            &crate::auth::format_timestamp(crate::auth::now()),
        )?;
        if self.legacy_probe_owner()?.as_deref() == Some(provider_id)
            || self.setting(SETTING_PROVIDER_MODELS)?.is_none()
        {
            self.set_setting(SETTING_PROVIDER_MODELS, &catalog)?;
            self.set_setting(SETTING_PROVIDER_CONTEXTS, &windows)?;
        }
        Ok(())
    }

    /// Record one provider's per-model request shapes and its dead models.
    ///
    /// Written by the same probe that wrote the catalogue, so the three keyed
    /// rows for one provider always describe one call. The legacy rows are not
    /// touched: they carry no place to put either fact.
    pub fn set_provider_probe_detail(
        &self,
        provider_id: &str,
        dialects: &std::collections::BTreeMap<String, core_domain::ApiDialect>,
        unreachable: &[String],
    ) -> Result<(), OnboardingError> {
        let shapes = serde_json::to_string(dialects).unwrap_or_else(|_| "{}".to_owned());
        self.set_setting(&Self::provider_dialects_key(provider_id), &shapes)?;
        let dead = serde_json::to_string(unreachable).unwrap_or_else(|_| "[]".to_owned());
        self.set_setting(&Self::provider_unreachable_key(provider_id), &dead)
    }

    /// One provider's per-model request shapes.
    pub fn provider_dialects(
        &self,
        provider_id: &str,
    ) -> Result<std::collections::BTreeMap<String, core_domain::ApiDialect>, OnboardingError> {
        let Some(json) = self.setting(&Self::provider_dialects_key(provider_id))? else {
            return Ok(std::collections::BTreeMap::new());
        };
        Ok(serde_json::from_str(&json).unwrap_or_default())
    }

    /// The request shape to use for one model on one provider.
    ///
    /// The model's own declared shape wins, because that is what the endpoint
    /// said about itself. With no declaration the provider's registry entry
    /// decides, and with neither the OpenAI shape stands: the same three-step
    /// fallback a turn has always had, now stated once.
    pub fn provider_dialect_for(
        &self,
        provider_id: &str,
        model: &str,
    ) -> Result<core_domain::ApiDialect, OnboardingError> {
        if let Some(dialect) = self.provider_dialects(provider_id)?.get(model) {
            return Ok(*dialect);
        }
        let entry = self
            .providers()
            .into_iter()
            .find(|provider| provider.id == provider_id);
        Ok(entry
            .as_ref()
            .map(crate::providers::dialect_of)
            .unwrap_or_default())
    }

    /// The models one provider listed and refused, as `id\treason` lines.
    pub fn provider_unreachable(&self, provider_id: &str) -> Result<Vec<String>, OnboardingError> {
        let Some(json) = self.setting(&Self::provider_unreachable_key(provider_id))? else {
            return Ok(Vec::new());
        };
        Ok(serde_json::from_str(&json).unwrap_or_default())
    }

    /// The model ids the last probe returned.
    ///
    /// This stays, and stays meaning exactly what it meant: the catalogue of the
    /// provider the legacy rows describe. A caller that knows which provider it
    /// is asking about uses [`Self::provider_catalog`] instead.
    pub fn provider_models(&self) -> Result<Vec<String>, OnboardingError> {
        let Some(json) = self.setting(SETTING_PROVIDER_MODELS)? else {
            return Ok(Vec::new());
        };
        Ok(serde_json::from_str(&json).unwrap_or_default())
    }

    /// The context windows the last probe declared, keyed by model id.
    pub fn provider_contexts(
        &self,
    ) -> Result<std::collections::BTreeMap<String, u64>, OnboardingError> {
        let Some(json) = self.setting(SETTING_PROVIDER_CONTEXTS)? else {
            return Ok(std::collections::BTreeMap::new());
        };
        Ok(serde_json::from_str(&json).unwrap_or_default())
    }

    /// Replace the stored context windows with what this probe declared.
    ///
    /// Empty is a real value: a gateway that publishes no windows clears the map
    /// rather than leaving a previous endpoint's numbers behind for a model id
    /// that survived the switch.
    pub fn set_provider_contexts(
        &self,
        contexts: &std::collections::BTreeMap<String, u64>,
    ) -> Result<(), OnboardingError> {
        let json = serde_json::to_string(contexts).unwrap_or_else(|_| "{}".to_owned());
        self.set_setting(SETTING_PROVIDER_CONTEXTS, &json)
    }

    /// Check that a model id is one the last probe published, without writing it.
    ///
    /// One rule, three callers: the wizard, the dashboard, and the device
    /// settings route. Splitting it out is what keeps a device from storing a
    /// model id the endpoint never offered.
    ///
    /// # Errors
    ///
    /// [`OnboardingError::ModelRequired`] for a blank id, and
    /// [`OnboardingError::ModelNotInProbe`] for an id outside the published list.
    pub fn check_model(&self, model: &str) -> Result<String, OnboardingError> {
        let model = model.trim();
        if model.is_empty() {
            return Err(OnboardingError::ModelRequired);
        }
        if !self.provider_models()?.iter().any(|known| known == model) {
            return Err(OnboardingError::ModelNotInProbe);
        }
        Ok(model.to_owned())
    }

    /// Check a model id against the primary provider's own catalogue.
    ///
    /// Same rule as [`Self::check_model`], asked of the provider a turn will
    /// actually resolve to. The difference is real on an install with two
    /// providers: a model the second provider published is a valid pin under
    /// this check and was refused under the old one.
    ///
    /// # Errors
    ///
    /// [`OnboardingError::ModelRequired`] for a blank id, and
    /// [`OnboardingError::ModelNotInProbe`] for an id outside the primary's list.
    pub fn check_primary_model(&self, model: &str) -> Result<String, OnboardingError> {
        let model = model.trim();
        if model.is_empty() {
            return Err(OnboardingError::ModelRequired);
        }
        if !self
            .primary_provider_catalog()?
            .iter()
            .any(|known| known == model)
        {
            return Err(OnboardingError::ModelNotInProbe);
        }
        Ok(model.to_owned())
    }

    /// Store the default model, which must be in the probe's list.
    ///
    /// The legacy probe's list, which is the right authority for the wizard: the
    /// step that writes this pin is the one that ran that probe. A caller that
    /// validates against something else - the primary provider's own catalogue,
    /// or a `provider/model` value a picker submitted - validates first and then
    /// calls [`Self::store_model`], so a value cannot be refused on the way in
    /// and then refused again by a check against a different list on the way out.
    pub fn set_model(&self, model: &str) -> Result<(), OnboardingError> {
        let model = self.check_model(model)?;
        self.store_model(&model)?;
        self.auth().audit("loopback", "setup.model", Some(&model))?;
        Ok(())
    }

    /// Store the default model verbatim, for a caller that has already checked it.
    ///
    /// No validation and no audit row: the value has been judged against the
    /// catalogue the caller holds, and that caller writes its own audit entry
    /// naming the surface the change came from. What stays here is the part every
    /// path shares - the write, and the fact that the models step is now done.
    pub fn store_model(&self, model: &str) -> Result<(), OnboardingError> {
        self.set_setting(SETTING_MODEL, model)?;
        self.complete_step(Step::Models)?;
        Ok(())
    }

    /// The stored default model.
    pub fn model(&self) -> Result<Option<String>, OnboardingError> {
        self.setting(SETTING_MODEL)
    }

    /// Store the permission tuple after validation.
    pub fn set_permissions(&self, permissions: Permissions) -> Result<(), OnboardingError> {
        self.set_setting(SETTING_SANDBOX_MODE, permissions.sandbox_mode.as_str())?;
        self.set_setting(
            SETTING_APPROVAL_POLICY,
            permissions.approval_policy.as_str(),
        )?;
        self.set_setting(
            SETTING_AUTOPILOT_CEILING,
            permissions.autopilot_ceiling.as_str(),
        )?;
        self.set_setting(SETTING_TELEMETRY, permissions.telemetry.as_str())?;
        if self.step_completed_at(Step::Permissions)?.is_none() {
            self.complete_step(Step::Permissions)?;
            self.auth().audit(
                "loopback",
                "setup.permissions",
                Some(permissions.sandbox_mode.as_str()),
            )?;
        }
        Ok(())
    }

    /// The stored permission tuple, defaulted where nothing was stored yet.
    pub fn permissions(&self) -> Result<Permissions, OnboardingError> {
        let read = |key: &str, fallback: &str| -> Result<String, OnboardingError> {
            Ok(self.setting(key)?.unwrap_or_else(|| fallback.to_owned()))
        };
        Ok(Permissions {
            sandbox_mode: SandboxMode::parse(&read(
                SETTING_SANDBOX_MODE,
                SandboxMode::DEFAULT.as_str(),
            )?)
            .unwrap_or(SandboxMode::DEFAULT),
            approval_policy: ApprovalPolicy::parse(&read(
                SETTING_APPROVAL_POLICY,
                ApprovalPolicy::DEFAULT.as_str(),
            )?)
            .unwrap_or(ApprovalPolicy::DEFAULT),
            autopilot_ceiling: AutopilotCeiling::parse(&read(
                SETTING_AUTOPILOT_CEILING,
                AutopilotCeiling::DEFAULT.as_str(),
            )?)
            .unwrap_or(AutopilotCeiling::DEFAULT),
            telemetry: Telemetry::parse(&read(SETTING_TELEMETRY, Telemetry::DEFAULT.as_str())?)
                .unwrap_or(Telemetry::DEFAULT),
        })
    }

    /// Whether the operator asked for the device listener.
    pub fn device_listener_enabled(&self) -> Result<bool, OnboardingError> {
        Ok(self
            .setting(SETTING_DEVICE_LISTENER)?
            .is_some_and(|value| value == "true"))
    }

    /// Record the device listener intent. The port is P8's to bind.
    pub fn set_device_listener(&self, enabled: bool) -> Result<(), OnboardingError> {
        self.set_setting(
            SETTING_DEVICE_LISTENER,
            if enabled { "true" } else { "false" },
        )?;
        self.auth().audit(
            "loopback",
            "dashboard.device_listener",
            Some(if enabled { "enabled" } else { "disabled" }),
        )?;
        Ok(())
    }

    /// Validate, create if needed, and store the workspace path.
    pub fn set_workspace(&self, input: &str) -> Result<PreparedWorkspace, OnboardingError> {
        let prepared = workspace::prepare(input)?;
        self.set_setting(SETTING_WORKSPACE, &prepared.path.display().to_string())?;
        self.complete_step(Step::Workspace)?;
        self.auth().audit(
            "loopback",
            "setup.workspace",
            Some(&prepared.path.display().to_string()),
        )?;
        Ok(prepared)
    }

    /// The stored workspace, or the documented default.
    pub fn workspace_path(&self) -> Result<PathBuf, OnboardingError> {
        match self.setting(SETTING_WORKSPACE)? {
            Some(path) => Ok(PathBuf::from(path)),
            None => Ok(workspace::default_workspace()?),
        }
    }

    /// How many devices are genuinely paired.
    ///
    /// "Paired" is not "a row exists". Minting a code registers a device row, and
    /// P6 stamps `paired_at` at registration, so counting rows would let the
    /// optional wizard step switch off approvals for a phone that never redeemed
    /// anything. The fact that unlocks `never` is a **redeemed** code or a live
    /// token: proof the other end completed the pairing.
    pub fn paired_device_count(&self) -> Result<usize, OnboardingError> {
        let count: i64 = self
            .db
            .with_conn(|conn| conn.query_row(PAIRED_DEVICE_COUNT_SQL, [], |row| row.get(0)))?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// Lock the wizard, move the daemon to `ready`, and write the audit rows.
    pub fn finish(&self) -> Result<(), OnboardingError> {
        self.ensure_wizard_open()?;
        let wizard = self.wizard()?;
        for step in Step::REQUIRED {
            if step == Step::Finish {
                continue;
            }
            if !wizard.completed(step) {
                return Err(OnboardingError::StepIncomplete {
                    step: step.as_str(),
                });
            }
        }
        // The claim stored the passphrase verifier as the admin credential, so
        // `eris setup --reset` can ask for the same value later.
        self.complete_step(Step::Finish)?;
        self.db.set_setup_state(SetupState::Ready)?;
        self.auth()
            .audit("loopback", "setup.finished", Some("state=ready"))?;
        Ok(())
    }

    /// `eris setup --reset`: verify the admin passphrase, reopen the wizard,
    /// retire every session, and mint a fresh passphrase.
    ///
    /// The provider, model, and permission rows are kept on purpose: a reset is
    /// about the operator losing the passphrase, not about forgetting that the
    /// endpoint is 9router. The wizard shows them pre-filled.
    pub fn reset(&self, admin_passphrase: &str) -> Result<FirstRun, OnboardingError> {
        if self.auth().admin_password_hash()?.is_none() {
            return Err(OnboardingError::AdminPassphraseRequired);
        }
        self.auth()
            .verify_admin_password(admin_passphrase)
            .map_err(|error| match error {
                crate::auth::AuthError::AdminPasswordRejected => {
                    OnboardingError::AdminPassphraseRejected
                }
                other => OnboardingError::Auth(other),
            })?;

        self.sessions().revoke_all()?;
        for step in Step::ALL {
            self.clear_setting(&step.key())?;
        }
        self.clear_setting(SETTING_PASSPHRASE_HASH)?;
        self.clear_setting(SETTING_PASSPHRASE_CREATED)?;
        self.db.set_setup_state(SetupState::Setup)?;
        self.auth().audit("loopback", "setup.reset", None)?;

        // Regenerate, because a reset is usually a lost passphrase.
        let value = passphrase::generate()?;
        let path = self.first_run_path();
        passphrase::write_first_run(&path, &value)?;
        let verifier = crate::auth::hash_secret(value.as_bytes())?;
        self.set_setting(SETTING_PASSPHRASE_HASH, &verifier)?;
        self.set_setting(
            SETTING_PASSPHRASE_CREATED,
            &crate::auth::format_timestamp(crate::auth::now()),
        )?;
        Ok(FirstRun {
            generated: Some(value),
            first_run_path: path,
            state: SetupState::Setup,
        })
    }

    /// `POST /login`: check the admin passphrase and, if it verifies, let the
    /// caller mint a browser session.
    ///
    /// This is [`SetupEngine::reset`]'s verification half with every mutation
    /// removed. It writes no setting, rewrites no passphrase, revokes no session,
    /// and does not read or touch the wizard, because signing in is not a
    /// lifecycle event. The only database write is the `auth.admin_password_rejected`
    /// audit row that [`AuthStore::verify_admin_password`] already writes on a
    /// failure.
    ///
    /// The value is normalized exactly as [`SetupEngine::accept_claim`] normalizes
    /// it before storing the verifier, so the six words work whether the operator
    /// types them in lower case or not. Only the Argon2id verifier is compared
    /// against; the passphrase itself is never stored, logged, or echoed.
    ///
    /// A daemon with no admin credential refuses with
    /// [`OnboardingError::AdminPassphraseRequired`], the same way `reset` does.
    pub fn login(&self, admin_passphrase: &str) -> Result<(), OnboardingError> {
        if self.auth().admin_password_hash()?.is_none() {
            return Err(OnboardingError::AdminPassphraseRequired);
        }
        self.auth()
            .verify_admin_password(&passphrase::normalize(admin_passphrase))
            .map_err(|error| match error {
                crate::auth::AuthError::AdminPasswordRejected => {
                    OnboardingError::AdminPassphraseRejected
                }
                other => OnboardingError::Auth(other),
            })
    }

    /// Whether the daemon is ready. Convenience for the web layer.
    pub fn is_ready(&self) -> Result<bool, OnboardingError> {
        Ok(self.state()? == SetupState::Ready)
    }

    /// The permission table for the current state.
    pub fn permission_rows(&self) -> Result<Vec<PermissionRow>, OnboardingError> {
        let permissions = self.permissions()?;
        let workspace = self.workspace_path()?;
        let paired = self.paired_device_count()? > 0;
        let ready = self.is_ready()?;
        Ok(permission_rows(
            permissions,
            &workspace.display().to_string(),
            paired,
            ready,
        ))
    }

    /// The directory the first-run file lives in, for the boot message.
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.paths.config_dir
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, SetupEngine) {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();
        let db = Db::open(root.join("state/eris.db")).expect("db opens");
        let paths = Paths::for_root(root.join("root"));
        (dir, SetupEngine::new(db, paths))
    }

    /// A database with the two-provider registry already stored.
    ///
    /// The chat layer's own tests need a real engine rather than a stub: what
    /// they check is that a model choice resolves its provider out of the
    /// registry, and a hand-built struct would only prove the test agrees with
    /// itself.
    pub(crate) fn fixture_for_chat() -> (tempfile::TempDir, SetupEngine) {
        let (dir, engine) = fixture();
        ready_engine(&engine);
        two_providers(&engine, "default");
        (dir, engine)
    }

    /// Walk the wizard to a ready daemon, the way the existing tests do: the
    /// passphrase is generated once, then claimed.
    fn ready_engine(engine: &SetupEngine) {
        let value = engine
            .ensure_first_run()
            .expect("first run")
            .generated
            .expect("a passphrase was generated");
        claim_through_permissions(engine, &value);
    }

    fn claim_through_permissions(engine: &SetupEngine, value: &str) {
        engine.accept_claim(value).expect("claim");
        engine
            .set_provider(
                "http://127.0.0.1:20128/v1",
                "env:NINEROUTER_API_KEY",
                &["ds/deepseek-v4-flash".to_owned()],
            )
            .expect("provider");
        engine.set_model("ds/deepseek-v4-flash").expect("model");
        engine
            .set_permissions(Permissions::DEFAULT)
            .expect("permissions");
    }

    #[test]
    fn a_fresh_database_is_in_setup_until_a_credential_exists() {
        let (_dir, engine) = fixture();
        assert_eq!(engine.state().expect("state"), SetupState::Setup);
        assert!(engine.reconcile_first_boot().is_ok());
        engine.db.set_setup_state(SetupState::Ready).expect("force");
        assert_eq!(
            engine.reconcile_first_boot().expect("reconcile"),
            SetupState::Setup,
            "ready without a credential is not ready"
        );
    }

    #[test]
    fn the_passphrase_is_generated_once_and_stored_only_as_a_verifier() {
        let (_dir, engine) = fixture();
        let first = engine.ensure_first_run().expect("first");
        let value = first.generated.clone().expect("generated");
        assert_eq!(value.split(' ').count(), WORD_COUNT);
        assert!(first.first_run_path.exists());

        let second = engine.ensure_first_run().expect("second");
        assert!(second.generated.is_none(), "it must not be regenerated");

        let stored = engine
            .setting(SETTING_PASSPHRASE_HASH)
            .expect("read")
            .expect("stored");
        assert!(stored.starts_with("$argon2id$"), "{stored}");
        assert!(
            !stored.contains(&value),
            "the verifier row must not contain the passphrase"
        );
    }

    #[test]
    fn claiming_is_case_and_space_insensitive_but_order_sensitive() {
        let (_dir, engine) = fixture();
        let run = engine.ensure_first_run().expect("first run");
        let value = run.generated.expect("generated");
        let shouted = format!("  {}  ", value.to_ascii_uppercase());
        engine
            .verify_passphrase(&shouted)
            .expect("normalized match");

        let mut words: Vec<&str> = value.split(' ').collect();
        words.reverse();
        let reversed = words.join(" ");
        assert_eq!(
            engine
                .verify_passphrase(&reversed)
                .expect_err("wrong order")
                .code(),
            "passphrase_rejected"
        );
    }

    #[test]
    fn the_wizard_is_server_side_and_survives_a_reopen() {
        let dir_tempfile = tempfile::tempdir().expect("temp dir");
        let root = dir_tempfile.path().to_path_buf();
        let db_path = root.join("state/eris.db");
        let paths = Paths::for_root(root.join("root"));
        let value = {
            let engine = SetupEngine::new(Db::open(&db_path).expect("open"), paths.clone());
            let value = engine
                .ensure_first_run()
                .expect("first run")
                .generated
                .expect("a passphrase was generated");
            claim_through_permissions(&engine, &value);
            engine
                .set_workspace(&root.join("ws").display().to_string())
                .expect("workspace");
            value
        };
        // A new process would open the same file. Progress is read from rows.
        let reopened = SetupEngine::new(Db::open(&db_path).expect("reopen"), paths);
        let wizard = reopened.wizard().expect("wizard");
        assert!(wizard.completed(Step::Claim));
        assert!(wizard.completed(Step::Provider));
        assert!(wizard.completed(Step::Models));
        assert!(wizard.completed(Step::Permissions));
        assert!(wizard.completed(Step::Workspace));
        assert!(!wizard.completed(Step::Finish));
        assert_eq!(wizard.next_step, Some(Step::Finish));
        reopened.verify_passphrase(&value).expect("still verifies");
    }

    #[test]
    fn a_step_out_of_order_is_refused() {
        let (_dir, engine) = fixture();
        engine.ensure_first_run().expect("first run");
        let refused = engine.require_step(Step::Models).expect_err("out of order");
        assert_eq!(refused.code(), "step_out_of_order");
        assert!(refused.to_string().contains("provider"), "{refused}");

        engine.complete_step(Step::Claim).expect("claim");
        engine
            .require_step(Step::Provider)
            .expect("provider is next and claim is done");
    }

    #[test]
    fn finish_refuses_an_open_step_and_then_locks_the_wizard() {
        let (_dir, engine) = fixture();
        let value = engine
            .ensure_first_run()
            .expect("first run")
            .generated
            .expect("a passphrase was generated");
        engine.accept_claim(&value).expect("claim");
        let refused = engine.finish().expect_err("provider is open");
        assert_eq!(refused.code(), "step_incomplete");

        claim_through_permissions(&engine, &value);
        let workspace = engine.paths().state_dir.join("ws");
        engine
            .set_workspace(&workspace.display().to_string())
            .expect("workspace");
        engine.finish().expect("finish");
        assert_eq!(engine.state().expect("state"), SetupState::Ready);
        assert_eq!(
            engine.ensure_wizard_open().expect_err("locked").code(),
            "wizard_locked"
        );
    }

    #[test]
    fn a_model_outside_the_probe_is_refused() {
        let (_dir, engine) = fixture();
        engine.ensure_first_run().expect("first run");
        engine.complete_step(Step::Claim).expect("claim");
        engine
            .set_provider(
                "http://127.0.0.1:20128/v1",
                "env:NINEROUTER_API_KEY",
                &["a".to_owned(), "b".to_owned()],
            )
            .expect("provider");
        assert_eq!(
            engine.set_model("c").expect_err("not probed").code(),
            "model_not_in_probe"
        );
        engine.set_model("b").expect("probed");
        assert_eq!(engine.model().expect("model").as_deref(), Some("b"));
    }

    #[test]
    fn a_key_value_is_refused_where_a_reference_belongs() {
        let (_dir, engine) = fixture();
        engine.ensure_first_run().expect("first run");
        engine.complete_step(Step::Claim).expect("claim");
        for bad in ["sk-live-abc", "", "file:/tmp/key"] {
            let refused = engine
                .set_provider("http://127.0.0.1:20128/v1", bad, &["a".to_owned()])
                .expect_err("refused");
            assert_eq!(refused.code(), "provider_key_ref_invalid", "{bad}");
        }
        let refused = engine
            .set_provider("file:///tmp/socket", "env:KEY", &["a".to_owned()])
            .expect_err("not http");
        assert_eq!(refused.code(), "provider_endpoint_invalid");
    }

    #[test]
    fn reset_needs_the_admin_passphrase_and_reopens_the_wizard() {
        let (_dir, engine) = fixture();
        let value = engine
            .ensure_first_run()
            .expect("first run")
            .generated
            .expect("a passphrase was generated");
        claim_through_permissions(&engine, &value);
        let workspace = engine.paths().state_dir.join("ws");
        engine
            .set_workspace(&workspace.display().to_string())
            .expect("workspace");
        engine.finish().expect("finish");
        // The claim stored the passphrase as the admin credential.
        let session = engine.sessions().mint().expect("session");
        engine
            .sessions()
            .verify(&session.cookie_value)
            .expect("live");

        assert_eq!(
            engine
                .reset("wrong passphrase value")
                .expect_err("wrong")
                .code(),
            "admin_passphrase_rejected"
        );
        let run = engine.reset(&value).expect("reset");
        assert_eq!(engine.state().expect("state"), SetupState::Setup);
        assert!(run.generated.is_some(), "a reset mints a fresh passphrase");
        assert!(
            !engine.wizard().expect("wizard").completed(Step::Claim),
            "the steps are cleared"
        );
        assert_eq!(
            engine.sessions().live_count().expect("live"),
            0,
            "a reset retires every browser session"
        );
        assert_eq!(
            engine.provider_base_url().expect("provider").as_deref(),
            Some("http://127.0.0.1:20128/v1"),
            "the endpoint survives a reset"
        );
    }

    #[test]
    fn the_permission_rows_follow_the_defaults() {
        let (_dir, engine) = fixture();
        let rows = engine.permission_rows().expect("rows");
        let approval = rows
            .iter()
            .find(|row| row.field == "approval_policy")
            .expect("approval row");
        assert_eq!(approval.value, "Ask me");
        let sandbox = rows
            .iter()
            .find(|row| row.field == "sandbox_mode")
            .expect("sandbox row");
        assert_eq!(sandbox.value, "Read only");
        assert!(
            sandbox.options.iter().all(|(value, _, _)| *value != "full"),
            "full is never offered by onboarding"
        );
    }

    #[test]
    fn a_minted_code_does_not_count_as_a_paired_device() {
        let (_dir, engine) = fixture();
        let value = engine
            .ensure_first_run()
            .expect("first run")
            .generated
            .expect("a passphrase was generated");
        claim_through_permissions(&engine, &value);

        let store = engine.auth();
        let device_id = store.register_device("Phone", "ios").expect("register");
        let issued = crate::auth::PairingService::new(engine.db().clone())
            .issue_for_device(&device_id)
            .expect("issue");

        // P6 stamps `paired_at` at registration, so the row alone must not be
        // enough to switch approvals off.
        assert_eq!(
            engine.paired_device_count().expect("count"),
            0,
            "a registered device with an unredeemed code is not paired"
        );

        crate::auth::PairingService::new(engine.db().clone())
            .redeem(issued.code())
            .expect("the code redeems once");
        assert_eq!(
            engine.paired_device_count().expect("count"),
            1,
            "a redeemed code is proof the other end completed the pairing"
        );
    }

    #[test]
    fn the_device_listener_intent_is_stored_off_by_default() {
        let (_dir, engine) = fixture();
        assert!(!engine.device_listener_enabled().expect("default"));
        engine.set_device_listener(true).expect("enable");
        assert!(engine.device_listener_enabled().expect("enabled"));
    }

    /// A registry with two providers, the second of them primary.
    fn two_providers(engine: &SetupEngine, primary: &str) {
        let registry = r#"[{"id":"default","name":"9router",
             "base_url":"http://127.0.0.1:20128/v1","key_ref":"env:NINEROUTER_API_KEY"},
            {"id":"commandcode","name":"Command Code",
             "base_url":"https://api.commandcode.ai/provider/v1",
             "key_ref":"env:COMMANDCODE_API_KEY"}]"#;
        crate::providers::parse(registry).expect("registry validates");
        engine
            .set_setting(crate::providers::SETTING_REGISTRY, registry)
            .expect("registry stores");
        engine
            .set_setting(crate::providers::SETTING_PRIMARY, primary)
            .expect("primary stores");
    }

    fn publish(engine: &SetupEngine, provider: &str, models: &[&str]) {
        let models: Vec<String> = models.iter().map(|id| (*id).to_owned()).collect();
        engine
            .set_provider_catalog(provider, &models, &std::collections::BTreeMap::new())
            .expect("catalogue stores");
    }

    #[test]
    fn a_second_providers_probe_does_not_overwrite_the_firsts() {
        let (_dir, engine) = fixture();
        ready_engine(&engine);
        publish(&engine, "commandcode", &["cc/model-one", "cc/model-two"]);

        assert_eq!(
            engine.provider_catalog("commandcode").expect("read"),
            vec!["cc/model-one".to_owned(), "cc/model-two".to_owned()]
        );
        // The provider the legacy probe was taken against still reads its own
        // list, and the legacy row itself is untouched.
        assert_eq!(
            engine.provider_catalog("default").expect("read"),
            vec!["ds/deepseek-v4-flash".to_owned()]
        );
        assert_eq!(
            engine.provider_models().expect("read"),
            vec!["ds/deepseek-v4-flash".to_owned()],
            "a second provider's catalogue must not masquerade as the first one's"
        );
    }

    #[test]
    fn the_primary_catalogue_follows_the_primary_not_the_legacy_row() {
        let (_dir, engine) = fixture();
        ready_engine(&engine);
        two_providers(&engine, "default");
        publish(&engine, "commandcode", &["cc/model-one"]);

        assert_eq!(
            engine.primary_provider_catalog().expect("read"),
            vec!["ds/deepseek-v4-flash".to_owned()],
            "while the default is primary, its own list is what a pin is checked against"
        );

        two_providers(&engine, "commandcode");
        assert_eq!(
            engine.primary_provider_catalog().expect("read"),
            vec!["cc/model-one".to_owned()]
        );
    }

    #[test]
    fn a_model_the_second_provider_published_is_a_valid_pin_once_it_is_primary() {
        let (_dir, engine) = fixture();
        ready_engine(&engine);
        two_providers(&engine, "default");
        publish(&engine, "commandcode", &["cc/model-one"]);

        // The legacy check asks the provider the legacy probe belongs to, which
        // is what an older install means by it.
        assert!(
            engine.check_model("cc/model-one").is_err(),
            "the default provider never published that id"
        );
        assert!(engine.check_model("ds/deepseek-v4-flash").is_ok());

        two_providers(&engine, "commandcode");
        assert_eq!(
            engine
                .check_primary_model("cc/model-one")
                .expect("accepted"),
            "cc/model-one"
        );
        assert!(
            engine.check_primary_model("ds/deepseek-v4-flash").is_err(),
            "the new primary never published the old provider's id"
        );
    }

    #[test]
    fn the_legacy_probe_seeds_the_provider_its_key_reference_names() {
        let (_dir, engine) = fixture();
        ready_engine(&engine);
        // The legacy rows describe 9router, and the seeded registry entry for
        // 9router is the one they belong to.
        let seeded = engine.migrate_provider_catalog().expect("seed");
        assert_eq!(seeded, vec!["default".to_owned()]);
        assert_eq!(
            engine.provider_catalog("default").expect("read"),
            vec!["ds/deepseek-v4-flash".to_owned()]
        );
        assert!(
            engine
                .provider_catalog("commandcode")
                .expect("read")
                .is_empty(),
            "a provider with no probe of its own is empty, not borrowed"
        );
        assert!(
            engine.migrate_provider_catalog().expect("seed").is_empty(),
            "seeding twice is a no-op rather than a second write"
        );
    }
}
