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

    /// The model ids the last probe returned.
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

    /// Store the default model, which must be in the probe's list.
    pub fn set_model(&self, model: &str) -> Result<(), OnboardingError> {
        let model = model.trim();
        if model.is_empty() {
            return Err(OnboardingError::ModelRequired);
        }
        if !self.provider_models()?.iter().any(|known| known == model) {
            return Err(OnboardingError::ModelNotInProbe);
        }
        self.set_setting(SETTING_MODEL, model)?;
        self.complete_step(Step::Models)?;
        self.auth().audit("loopback", "setup.model", Some(model))?;
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
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, SetupEngine) {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();
        let db = Db::open(root.join("state/eris.db")).expect("db opens");
        let paths = Paths::for_root(root.join("root"));
        (dir, SetupEngine::new(db, paths))
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
}
