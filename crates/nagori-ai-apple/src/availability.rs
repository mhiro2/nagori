//! Apple Intelligence availability detection, plus the mock fixtures that let
//! CI exercise every unavailable branch on hosts without an Apple Intelligence
//! environment.

/// Availability of Apple's on-device text generation.
///
/// The first four variants mirror `SystemLanguageModel.availability`
/// (`available` plus the three confirmed `UnavailableReason`s).
/// [`AppleAvailability::RateLimited`] is *not* an
/// availability state on the OS — it is a background `GenerationError` kept as
/// an absorption lane — so it is only ever produced by the mock fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppleAvailability {
    /// Apple Intelligence is enabled and the model is ready.
    Available,
    /// The device is not eligible (e.g. pre-M1 silicon, or a non-Apple host).
    DeviceNotEligible,
    /// Apple Intelligence has not been enabled in System Settings.
    AppleIntelligenceNotEnabled,
    /// The model is still downloading or otherwise not yet ready.
    ModelNotReady,
    /// Background asset generation is rate limited (mock-only absorption lane).
    RateLimited,
    /// An availability state this build does not recognise.
    Unknown,
}

impl AppleAvailability {
    /// Whether text generation can be attempted.
    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }

    /// Maps the integer code returned by the Swift availability probe.
    /// Unrecognised codes (including the runtime's `255` unknown sentinel)
    /// collapse to [`AppleAvailability::Unknown`]. macOS-only: it only has a
    /// caller (the Swift bridge) on Apple platforms.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub(crate) const fn from_probe_code(code: i32) -> Self {
        match code {
            0 => Self::Available,
            1 => Self::DeviceNotEligible,
            2 => Self::AppleIntelligenceNotEnabled,
            3 => Self::ModelNotReady,
            _ => Self::Unknown,
        }
    }
}

/// The reason a [`AvailabilitySource::Mock`] fixture should report. Covers the
/// happy path plus the four unavailable states CI must be able to inject from
/// Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockReason {
    /// Pretend Apple Intelligence is available.
    Available,
    /// Pretend the device is not eligible.
    DeviceNotEligible,
    /// Pretend Apple Intelligence is disabled in System Settings.
    AppleIntelligenceNotEnabled,
    /// Pretend the model is still downloading.
    ModelNotReady,
    /// Pretend background asset generation is rate limited.
    RateLimited,
}

impl From<MockReason> for AppleAvailability {
    fn from(reason: MockReason) -> Self {
        match reason {
            MockReason::Available => Self::Available,
            MockReason::DeviceNotEligible => Self::DeviceNotEligible,
            MockReason::AppleIntelligenceNotEnabled => Self::AppleIntelligenceNotEnabled,
            MockReason::ModelNotReady => Self::ModelNotReady,
            MockReason::RateLimited => Self::RateLimited,
        }
    }
}

/// Where an availability result comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvailabilitySource {
    /// Probe the live OS via the Swift bridge (macOS only; other platforms
    /// report [`AppleAvailability::DeviceNotEligible`]).
    Real,
    /// Substitute a fixed result, for tests and CI.
    Mock(MockReason),
}

/// Resolves availability from the given `source`.
///
/// ```
/// use nagori_ai_apple::{probe, AvailabilitySource, MockReason, AppleAvailability};
///
/// let result = probe(AvailabilitySource::Mock(MockReason::ModelNotReady));
/// assert_eq!(result, AppleAvailability::ModelNotReady);
/// assert!(!result.is_available());
/// ```
#[must_use]
pub fn probe(source: AvailabilitySource) -> AppleAvailability {
    match source {
        AvailabilitySource::Mock(reason) => reason.into(),
        AvailabilitySource::Real => real_availability(),
    }
}

#[cfg(target_os = "macos")]
fn real_availability() -> AppleAvailability {
    crate::bridge::probe_real_availability()
}

#[cfg(not(target_os = "macos"))]
const fn real_availability() -> AppleAvailability {
    // No Apple Intelligence off Apple platforms.
    AppleAvailability::DeviceNotEligible
}

#[cfg(test)]
mod tests {
    use super::{AppleAvailability, AvailabilitySource, MockReason, probe};

    #[test]
    fn mock_maps_every_reason() {
        let cases = [
            (MockReason::Available, AppleAvailability::Available),
            (
                MockReason::DeviceNotEligible,
                AppleAvailability::DeviceNotEligible,
            ),
            (
                MockReason::AppleIntelligenceNotEnabled,
                AppleAvailability::AppleIntelligenceNotEnabled,
            ),
            (MockReason::ModelNotReady, AppleAvailability::ModelNotReady),
            (MockReason::RateLimited, AppleAvailability::RateLimited),
        ];
        for (reason, expected) in cases {
            assert_eq!(probe(AvailabilitySource::Mock(reason)), expected);
        }
    }

    #[test]
    fn only_available_is_available() {
        assert!(AppleAvailability::Available.is_available());
        for unavailable in [
            AppleAvailability::DeviceNotEligible,
            AppleAvailability::AppleIntelligenceNotEnabled,
            AppleAvailability::ModelNotReady,
            AppleAvailability::RateLimited,
            AppleAvailability::Unknown,
        ] {
            assert!(!unavailable.is_available());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn probe_code_mapping() {
        assert_eq!(
            AppleAvailability::from_probe_code(0),
            AppleAvailability::Available
        );
        assert_eq!(
            AppleAvailability::from_probe_code(3),
            AppleAvailability::ModelNotReady
        );
        assert_eq!(
            AppleAvailability::from_probe_code(255),
            AppleAvailability::Unknown
        );
        assert_eq!(
            AppleAvailability::from_probe_code(-1),
            AppleAvailability::Unknown
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn real_probe_off_apple_is_device_not_eligible() {
        assert_eq!(
            probe(AvailabilitySource::Real),
            AppleAvailability::DeviceNotEligible
        );
    }
}
