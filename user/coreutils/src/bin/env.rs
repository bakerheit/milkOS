//! env                         print the environment
//! env [-i] [VAR=value...] CMD ARGS...   run CMD with a changed environment
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let mut a = &args.argv[1..];
    let mut env: Vec<String> = args.envp.iter().map(|s| String::from(*s)).collect();
    if a.first() == Some(&"-i") {
        env.clear();
        a = &a[1..];
    }
    while let Some(kv) = a.first().filter(|s| s.contains('=')) {
        let key = kv.split('=').next().unwrap();
        env.retain(|e| e.split('=').next() != Some(key));
        env.push(String::from(*kv));
        a = &a[1..];
    }
    if a.is_empty() && rt::table::requested(args) {
        use rt::table::{Table, Ty, Val};
        let mut t = Table::new(&[("name", Ty::Str), ("value", Ty::Str)]);
        for e in env.iter().filter(|e| !e.starts_with("MILK_TABLE=")) {
            let (k, v) = e.split_once('=').unwrap_or((e, ""));
            t.push(alloc::vec![Val::str(k), Val::str(v)]);
        }
        let mut out = rt::io::Out::stdout();
        t.write(&mut out);
        out.flush();
        return 0;
    }
    if a.is_empty() {
        let mut out = rt::io::Out::stdout();
        for e in &env {
            out.line(e);
        }
        out.flush();
        return 0;
    }
    let path = if a[0].contains('/') {
        String::from(a[0])
    } else {
        alloc::format!("/Commands/{}", a[0])
    };
    let envs: Vec<&str> = env.iter().map(|s| s.as_str()).collect();
    let e = rt::sys::execve(&path, a, &envs);
    rt::eprintln!("env: {}: {e}", a[0]);
    127
}
