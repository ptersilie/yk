#![feature(lazy_cell)]

mod hwtracer_ykpt;

use std::{
    collections::HashMap,
    env,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::LazyLock,
};
use ykbuild::ykllvm_bin;

pub use yktracec::promote::__yk_lookup_promote_usize;

const TEMPDIR_SUBST: &str = "%%TEMPDIR%%";
pub static EXTRA_LINK: LazyLock<HashMap<&'static str, Vec<ExtraLinkage>>> = LazyLock::new(|| {
    let mut map = HashMap::new();

    // These tests get an extra, separately compiled (thus opaque to LTO), object file linked in.
    for test_file in &[
        "call_ext_in_obj.c",
        "unmapped_setjmp.c",
        "loopy_funcs_not_inlined_by_default.c",
        "not_loopy_funcs_inlined_by_default.c",
        "reentrant.c",
        "unroll_safe_implies_noinline.c",
        "unroll_safe_inlines.c",
        "yk_unroll_safe_vs_yk_outline.c",
    ] {
        map.insert(
            *test_file,
            vec![ExtraLinkage::new(
                "%%TEMPDIR%%/call_me.o",
                ykllvm_bin("clang").to_owned(),
                &[
                    "-I../ykcapi",
                    "-c",
                    "-O0",
                    "extra_linkage/call_me.c",
                    "-o",
                    "%%TEMPDIR%%/call_me.o",
                ],
            )],
        );
    }
    map.insert(
        "pt_zero_len_call.c",
        vec![ExtraLinkage::new(
            "%%TEMPDIR%%/pt_zero_len_call.o",
            ykllvm_bin("clang").to_owned(),
            &[
                "-c",
                "extra_linkage/pt_zero_len_call.s",
                "-o",
                "%%TEMPDIR%%/pt_zero_len_call.o",
            ],
        )],
    );
    map
});

/// Describes an extra object file to link to a C test.
pub struct ExtraLinkage<'a> {
    /// The name of the object file to be generated.
    output_file: &'a str,
    /// The path to the binary we want to run.
    gen_bin: PathBuf,
    /// Arguments to the binary.
    gen_args: &'a [&'a str],
}

impl<'a> ExtraLinkage<'a> {
    pub fn new(output_file: &'a str, gen_bin: PathBuf, gen_args: &'a [&'a str]) -> Self {
        Self {
            output_file,
            gen_bin,
            gen_args,
        }
    }

    /// Run the command to generate the object in `tempdir` and return the absolute path to the
    /// generated object.
    pub fn generate_obj(&self, tempdir: &Path) -> PathBuf {
        let mut cmd = Command::new(&self.gen_bin);
        let tempdir_s = tempdir.to_str().unwrap();
        for arg in self.gen_args.iter() {
            cmd.arg(arg.replace(TEMPDIR_SUBST, tempdir_s));
        }
        let out = match cmd.output() {
            Ok(x) => x,
            Err(e) => panic!("Error when running {:?} {:?}", cmd, e),
        };
        assert!(tempdir.exists());
        if !out.status.success() {
            io::stdout().write_all(&out.stdout).unwrap();
            io::stderr().write_all(&out.stderr).unwrap();
            panic!();
        }
        let mut ret = PathBuf::from(tempdir);
        ret.push(&self.output_file.replace(TEMPDIR_SUBST, tempdir_s));
        ret
    }
}

/// Make a compiler command that compiles `src` to `exe` using the optimisation flag `opt`.
/// `extra_objs` is a collection of other object files to link.
///
/// If `patch_cp` is `false` then the argument to patch the control point is omitted.
pub fn mk_compiler(
    compiler: &Path,
    exe: &Path,
    src: &Path,
    opt: &str,
    extra_objs: &[PathBuf],
    patch_cp: bool,
) -> Command {
    let mut compiler = Command::new(compiler);

    let yk_config = [
        &env::var("CARGO_MANIFEST_DIR").unwrap(),
        "..",
        "bin",
        "yk-config",
    ]
    .iter()
    .collect::<PathBuf>();

    #[cfg(cargo_profile = "debug")]
    let mode = "debug";
    #[cfg(cargo_profile = "release")]
    let mode = "release";

    let prelink_args = env::var("PRELINK_PASSES").unwrap_or_default();
    let postlink_args = env::var("POSTLINK_PASSES").unwrap_or_default();

    let mut yk_config_args = vec![mode];

    if !prelink_args.is_empty() {
        yk_config_args.push("--prelink-pipeline");
        yk_config_args.push(&prelink_args);
    }
    yk_config_args.push("--cflags");
    yk_config_args.push("--cppflags");
    if !postlink_args.is_empty() {
        yk_config_args.push("--postlink-pipeline");
        yk_config_args.push(&postlink_args);
    }
    yk_config_args.push("--ldflags");
    yk_config_args.push("--libs");

    let yk_config_out = Command::new(yk_config)
        .args(&yk_config_args)
        .output()
        .expect("failed to execute yk-config");
    if !yk_config_out.status.success() {
        io::stderr().write_all(&yk_config_out.stderr).ok();
        panic!("yk-config exited with non-zero status");
    }

    let mut yk_flags = String::from_utf8(yk_config_out.stdout).unwrap();
    if !patch_cp {
        yk_flags = yk_flags.replace("-Wl,--mllvm=--yk-patch-control-point", "");
    }

    // yk-config never returns arguments containing spaces, so we can split by space here. If this
    // ever changes, then we should build arguments as an "unparsed" string and parse that to `sh
    // -c` and let the shell do the parsing.
    let yk_flags = yk_flags.trim().split(' ');
    compiler.args(yk_flags);

    compiler.args(extra_objs);
    compiler.args([
        opt,
        // If this is a debug build, include debug info in the test binary.
        #[cfg(debug_assertions)]
        "-g",
        // Be strict.
        "-Werror",
        "-Wall",
        // Some tests are multi-threaded via the pthread API.
        "-pthread",
        // The input and output files.
        "-o",
        exe.to_str().unwrap(),
        src.to_str().unwrap(),
    ]);
    compiler
}

/// Check the `std::process::Output` of a `std::process::Command`, printing the output and
/// panicking on non-zero exit status.
pub fn check_output(out: &Output) {
    if !out.status.success() {
        println!("{}", std::str::from_utf8(&out.stdout).unwrap());
        eprintln!("{}", std::str::from_utf8(&out.stderr).unwrap());
        panic!();
    }
}
