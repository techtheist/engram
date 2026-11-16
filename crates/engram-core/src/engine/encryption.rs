//! At-rest encryption of the graph and history stores.

use super::*;

impl Engine {
    /// The daemon's desired at-rest encryption per store: `Some(true)` sealed,
    /// `Some(false)` plaintext, `None` leave as recorded. Intent only; each store's
    /// meta holds the truth.
    pub fn set_desired_encryption(&mut self, graph: Option<bool>, history: Option<bool>) {
        self.desired_graph_enc = graph;
        self.desired_history_enc = history;
    }

    /// Decrypt one stored string for a reader. The store driver normally
    /// opens sealed rows itself; this is the legacy fallback for strings
    /// sealed by 0.8.x paths — the key loads lazily, only when a sealed
    /// string is actually met.
    pub(super) fn unseal_str(&self, s: &str) -> String {
        if !crate::seal::is_sealed(s) {
            return s.to_string();
        }
        self.legacy_key
            .get_or_init(crate::seal::SealKey::load_or_create)
            .as_ref()
            .and_then(|k| k.unseal(s))
            .unwrap_or_else(|| crate::seal::SEALED_PLACEHOLDER.to_string())
    }

    /// Reconcile the CURATED store with the daemon's desired encryption
    /// — run at open, after `ensure_*`. Returns rows reprocessed.
    pub fn ensure_encryption(&self) -> Result<usize> {
        reconcile_encryption(self.store.as_ref(), self.desired_graph_enc, &mut |_, _| {})
    }

    /// The recorded at-rest states — `(curated, history)`; history is `None`
    /// while the layer is closed. For `/encryption` and doctor.
    pub fn encryption_states(
        &self,
    ) -> (
        crate::seal::EncryptionState,
        Option<crate::seal::EncryptionState>,
    ) {
        (
            self.store.encryption_state(),
            self.with_history(|s| s.encryption_state()),
        )
    }

    /// Encrypt or decrypt a whole store now (`target`: "graph" | "history"), with
    /// progress callbacks; returns rows reprocessed. Killed midway, the next
    /// reconcile finishes.
    pub fn set_store_encryption(
        &self,
        target: &str,
        enable: bool,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<usize> {
        let n = match target {
            "graph" => reconcile_encryption(self.store.as_ref(), Some(enable), progress)?,
            "history" => self
                .with_history(|s| reconcile_encryption(s, Some(enable), progress))
                .transpose()?
                .ok_or_else(|| {
                    crate::Error::Config(
                        "the history layer is not open — enable session recording first".into(),
                    )
                })?,
            other => {
                return Err(crate::Error::Config(format!(
                    "unknown encryption target {other:?} — graph | history"
                )));
            }
        };
        self.audit(
            "encryption_changed",
            "graph",
            "",
            Some(format!(
                "{target} → {} ({n} rows reprocessed)",
                if enable { "sealed" } else { "plaintext" }
            )),
            None,
            None,
            None,
        )?;
        self.notify(ChangeEvent::ConfigChanged);
        Ok(n)
    }
}

/// Drive one store from its recorded encryption state to the desired one
///. `None` desired = hands off (library engines and tests never
/// migrate or touch the keyring). The mid-flight state persists BEFORE the
/// rewrite, so new writes already conform and a crash resumes cleanly.
pub(super) fn reconcile_encryption(
    store: &dyn Store,
    desired: Option<bool>,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<usize> {
    use crate::seal::EncryptionState as S;
    let target = match desired {
        Some(true) => S::Sealed,
        Some(false) => S::Plaintext,
        None => return Ok(0),
    };
    if store.encryption_state() == target {
        return Ok(0);
    }
    store.set_encryption_state(if target == S::Sealed {
        S::Sealing
    } else {
        S::Unsealing
    })?;
    let n = store.reseal_all(progress)?;
    store.set_encryption_state(target)?;
    Ok(n)
}
