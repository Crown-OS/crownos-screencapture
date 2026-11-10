//! Constant-quality rate control for each encoder FFmpeg can open.

use crownos_config::schema::RecordingQuality;
use ffmpeg_next::Dictionary;

use crate::record::RecordError;

/// How one encoder holds a constant quality, with its quantiser from Low to Lossless.
struct ConstantQuality {
    rate_mode: Option<(&'static str, &'static str)>,
    quantizer: &'static str,
    steps: [u8; 4],
}

impl ConstantQuality {
    fn step(&self, quality: RecordingQuality) -> u8 {
        let index = match quality {
            RecordingQuality::Low => 0,
            RecordingQuality::Medium => 1,
            RecordingQuality::High => 2,
            RecordingQuality::Lossless => 3,
        };
        self.steps[index]
    }
}

const VAAPI_QP: ConstantQuality = ConstantQuality {
    rate_mode: Some(("rc_mode", "CQP")),
    quantizer: "qp",
    steps: [32, 27, 23, 19],
};

const VAAPI_AV1: ConstantQuality = ConstantQuality {
    rate_mode: Some(("rc_mode", "CQP")),
    quantizer: "global_quality",
    steps: [150, 124, 100, 76],
};

const VULKAN_QP: ConstantQuality = ConstantQuality {
    rate_mode: Some(("rc_mode", "cqp")),
    quantizer: "qp",
    steps: [32, 27, 23, 19],
};

const VULKAN_AV1: ConstantQuality = ConstantQuality {
    rate_mode: Some(("rc_mode", "cqp")),
    quantizer: "qp",
    steps: [150, 124, 100, 76],
};

const X264: ConstantQuality = ConstantQuality {
    rate_mode: None,
    quantizer: "crf",
    steps: [28, 23, 19, 16],
};

const X265: ConstantQuality = ConstantQuality {
    rate_mode: None,
    quantizer: "crf",
    steps: [30, 26, 22, 19],
};

const SVT_AV1: ConstantQuality = ConstantQuality {
    rate_mode: None,
    quantizer: "crf",
    steps: [40, 35, 30, 24],
};

fn constant_quality(encoder: &str) -> Option<&'static ConstantQuality> {
    match encoder {
        "h264_vaapi" | "hevc_vaapi" => Some(&VAAPI_QP),
        "av1_vaapi" => Some(&VAAPI_AV1),
        "h264_vulkan" | "hevc_vulkan" => Some(&VULKAN_QP),
        "av1_vulkan" => Some(&VULKAN_AV1),
        "libx264" => Some(&X264),
        "libx265" => Some(&X265),
        "libsvtav1" => Some(&SVT_AV1),
        _ => None,
    }
}

/// Adds the options that hold `encoder` at a constant `quality`.
pub(crate) fn add_constant_quality(
    options: &mut Dictionary<'_>,
    encoder: &'static str,
    quality: RecordingQuality,
) -> Result<(), RecordError> {
    let rate = constant_quality(encoder).ok_or(RecordError::NoEncoder(encoder))?;
    if let Some((key, value)) = rate.rate_mode {
        options.set(key, value);
    }
    options.set(rate.quantizer, &rate.step(quality).to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENCODERS: [&str; 9] = [
        "h264_vaapi",
        "hevc_vaapi",
        "av1_vaapi",
        "h264_vulkan",
        "hevc_vulkan",
        "av1_vulkan",
        "libx264",
        "libx265",
        "libsvtav1",
    ];

    #[test]
    fn quantiser_falls_as_quality_rises() {
        for encoder in ENCODERS {
            let steps = constant_quality(encoder).map(|rate| rate.steps);
            assert!(
                steps.is_some_and(|steps| steps.is_sorted_by(|coarser, finer| coarser > finer)),
                "{encoder}"
            );
        }
    }

    #[test]
    fn emits_rate_mode_and_quantiser() {
        let options_for = |encoder| {
            let mut options = Dictionary::new();
            add_constant_quality(&mut options, encoder, RecordingQuality::High).map(|()| options)
        };
        let expected = [
            ("h264_vaapi", Some("CQP"), "qp", "23"),
            ("hevc_vaapi", Some("CQP"), "qp", "23"),
            ("av1_vaapi", Some("CQP"), "global_quality", "100"),
            ("h264_vulkan", Some("cqp"), "qp", "23"),
            ("hevc_vulkan", Some("cqp"), "qp", "23"),
            ("av1_vulkan", Some("cqp"), "qp", "100"),
            ("libx264", None, "crf", "19"),
            ("libx265", None, "crf", "22"),
            ("libsvtav1", None, "crf", "30"),
        ];
        for (encoder, rate_mode, quantizer, value) in expected {
            let options = options_for(encoder);
            assert!(options.is_ok(), "{encoder}");
            if let Ok(options) = options {
                assert_eq!(options.get("rc_mode"), rate_mode, "{encoder}");
                assert_eq!(options.get(quantizer), Some(value), "{encoder}");
            }
        }
        assert!(options_for("mpeg4").is_err());
    }
}
