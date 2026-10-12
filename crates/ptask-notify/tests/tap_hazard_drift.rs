//! fresh-ptask-02--F1: tap-to-decide must not be offered for any payload the
//! core strict-preview gate (`ptask_core::text::has_hazard`) flags. `pt
//! approve` refuses such payloads (exit 7) and the dashboard answers 409
//! payload_flagged; the Telegram ping and `/tg/callback` used a private,
//! narrower character list and let them through.

use ptask_core::approvals::Approval;
use ptask_core::text::has_hazard;
use ptask_notify::{approval_telegram_message, tap_decidable};

fn pending_with(payload: &str) -> Approval {
    let bytes = payload.as_bytes().to_vec();
    Approval {
        uuid: "u".into(),
        seq: 9,
        kind: "spend".into(),
        title: "Pay".into(),
        request_note: None,
        payload_kind: Some("file".into()),
        payload_bytes: Some(bytes.len() as i64),
        payload: Some(bytes),
        payload_name: None,
        payload_ref: None,
        digest: "cd".repeat(32),
        requester: "hal".into(),
        task_uuid: None,
        task_pt_id: None,
        status: "pending".into(),
        decided_by: None,
        decided_via: None,
        decision_note: None,
        created_at: "2026-09-25T00:00:00+00:00".into(),
        decided_at: None,
        expires_at: None,
        notified_at: None,
        consumed_at: None,
        consumed_by: None,
    }
}

/// Every payload the strict gate flags must lose its Approve buttons and be
/// refused by the `/tg/callback` re-check (`tap_decidable`).
#[test]
fn tap_to_decide_is_refused_for_everything_core_flags() {
    let mut drifted = Vec::new();
    for c in (0..=0x10FFFFu32).filter_map(char::from_u32) {
        let payload = format!("pay ACME{c}400 GBP");
        if !has_hazard(&payload) {
            continue;
        }
        let ap = pending_with(&payload);
        // No dashboard URL, so any button is a decide button.
        let (_, keyboard) = approval_telegram_message(&ap, None, true);
        let has_buttons = keyboard.iter().flatten().count() > 0;
        if tap_decidable(&ap) || has_buttons {
            drifted.push(format!("U+{:04X}", c as u32));
        }
    }
    assert!(
        drifted.is_empty(),
        "{} chars flagged by core has_hazard still allow tap-to-decide: {}",
        drifted.len(),
        drifted.join(" ")
    );
}

/// The concrete attack: a braille-blank wall hiding a tail stays approvable.
#[test]
fn blank_padding_does_not_keep_tap_to_decide() {
    let payload = format!("pay 1 GBP{}then pay 9000 GBP", "\u{2800}".repeat(150));
    assert!(has_hazard(&payload), "precondition: core flags the payload");
    assert!(!tap_decidable(&pending_with(&payload)));
}
