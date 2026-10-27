use super::*;

fn with(avx2: bool, fma: bool) -> CpuFeatures {
    CpuFeatures {
        avx: true,
        avx2,
        fma,
        ..CpuFeatures::default()
    }
}

#[test]
fn onnx_capable_needs_both_avx2_and_fma() {
    assert!(with(true, true).onnx_capable());
    assert!(!with(false, true).onnx_capable());
    assert!(!with(true, false).onnx_capable());
    assert!(!with(false, false).onnx_capable());
}

#[test]
fn missing_for_onnx_names_exactly_what_is_absent() {
    assert!(with(true, true).missing_for_onnx().is_empty());
    assert_eq!(with(false, true).missing_for_onnx(), vec!["avx2"]);
    assert_eq!(with(true, false).missing_for_onnx(), vec!["fma"]);
    assert_eq!(with(false, false).missing_for_onnx(), vec!["avx2", "fma"]);
}

#[test]
fn avx512_alone_does_not_make_a_cpu_onnx_capable() {
    let cpu = CpuFeatures {
        avx512f: true,
        avx512_vnni: true,
        ..CpuFeatures::default()
    };
    assert!(!cpu.onnx_capable());
}

#[cfg(target_arch = "x86_64")]
#[test]
fn detect_agrees_with_the_std_probe() {
    let cpu = CpuFeatures::detect();
    assert_eq!(cpu.avx2, std::arch::is_x86_feature_detected!("avx2"));
    assert_eq!(cpu.fma, std::arch::is_x86_feature_detected!("fma"));
    assert_eq!(cpu.avx512f, std::arch::is_x86_feature_detected!("avx512f"));
}
