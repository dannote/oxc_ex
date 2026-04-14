mod bundle;
mod error;
mod imports;
mod options;
mod parse;

mod atoms {
    rustler::atoms! {
        ok,
        error,
        atom_static = "static",
        dynamic,
        import,
        export,
        export_all,
    }
}

rustler::init!("Elixir.OXC.Native");
