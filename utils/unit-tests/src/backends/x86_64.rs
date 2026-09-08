//! Unit tests for the `tir-x86_64` backend's public API.

use tir::backend::abi::ValueKind;
use tir_x86_64::{Feature, TargetConfig};

use super::support::{numbers, pass_seq};

#[test]
fn x86_64_target_enables_required_features() {
    let config = TargetConfig::parse("x86_64", None, None).unwrap();
    assert_eq!(
        config.features(),
        &[Feature::X86, Feature::X86_64, Feature::SSE, Feature::SSE2,]
    );
    assert!(TargetConfig::parse("x86", None, None).is_err());
}

#[test]
fn generated_abi_matches_sysv_register_convention() {
    let target = tir::backend::select_target("x86_64", None, None).unwrap();
    let abi = target.abi();
    let int_args = pass_seq(abi.args, ValueKind::Int);

    assert_eq!(abi.name, "sysv");
    assert_eq!(abi.sp, (int_args.regs[0].0, 4));
    assert_eq!(abi.ra, None);
    assert_eq!(abi.fp, Some((int_args.regs[0].0, 5)));
    assert_eq!(abi.stack.align, 16);
    assert_eq!(abi.stack.slot_size, 8);
    assert_eq!(abi.stack.save_style, tir::backend::abi::SaveStyle::PushPop);
    assert_eq!(numbers(int_args.regs), vec![7, 6, 2, 1, 8, 9]);
    assert_eq!(numbers(pass_seq(abi.rets, ValueKind::Int).regs), vec![0, 2]);
    assert_eq!(
        numbers(pass_seq(abi.args, ValueKind::Float).regs),
        (0..=7).collect::<Vec<_>>()
    );
    assert_eq!(
        numbers(pass_seq(abi.rets, ValueKind::Float).regs),
        vec![0, 1]
    );
    assert_eq!(numbers(abi.callee_saved), vec![3, 5, 12, 13, 14, 15]);
}
