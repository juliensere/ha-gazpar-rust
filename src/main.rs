//! Point d'entrée du binaire `ha-gazpar` : transmet les arguments à la
//! bibliothèque et sort avec le code qu'elle renvoie. Rien d'autre.

#![forbid(unsafe_code)]

fn main() {
    std::process::exit(ha_gazpar::cli::main(std::env::args_os().skip(1)));
}
