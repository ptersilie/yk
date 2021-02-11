// Code shared by `ykrt/build.rs` and `xtask/src/main.rs`.

/// Determine what kind of tracing the user has requested at compile time, by looking at RUSTFLAGS.
/// Ideally, we'd have the user set another environment variable, and then set RUSTFLAGS
/// accordingly, but you cant' set arbitrary RUSTFLAGS from build.rs.
fn find_tracing_kind(rustflags: &str) -> String {
    let re = Regex::new(r"-C\s*tracer=([a-z]*)").unwrap();
    let mut cgs = re.captures_iter(&rustflags);
    let tracing_kind = if let Some(caps) = cgs.next() {
        caps.get(1).unwrap().as_str()
    } else {
        panic!("Please choose a tracer by setting `RUSTFLAGS=\"-C tracer=<kind>\"`.");
    };
    if cgs.next().is_some() {
        panic!("`-C tracer=<kind>` was specified more than once in $RUSTFLAGS");
    }
    tracing_kind.to_owned()
}

/// Given the RUSTFLAGS for the external workspace, make flags for the internal one.
fn make_internal_rustflags(rustflags: &str) -> String {
    // Remove `-C tracer=<kind>`, as this would stifle optimisations.
    let re = Regex::new(r"-C\s*tracer=[a-z]*").unwrap();
    let mut int_rustflags = re.replace_all(rustflags, "").to_string();

    // Set the tracermode cfg macro, but without changing anything relating to code generation.
    let tracing_kind = find_tracing_kind(&rustflags);
    int_rustflags.push_str(&format!(" --cfg tracermode=\"{}\"", tracing_kind));
    int_rustflags
}
