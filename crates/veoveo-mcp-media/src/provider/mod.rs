//! The generation provider. Its identity stays inside this module: the rest of Media sees
//! provider-neutral models, predictions, outcomes and bills.
mod client;
mod events;
pub mod webhook;
pub mod wire;

pub use client::{
    BILLING_TIMEOUT, CANCEL_TIMEOUT, ProviderClient, ProviderError, QUOTE_TIMEOUT,
    REGISTRY_TIMEOUT, STATUS_TIMEOUT, SUBMIT_TIMEOUT,
};
pub use events::{
    BillingOutcome, ProviderWebhooks, WEBHOOK_TOLERANCE_SECS, billing_key, billing_provider,
    cancel_prediction, check_billing, check_status, generation_key, generation_provider,
    prediction_of,
};
