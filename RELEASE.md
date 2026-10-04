# Beta installation

The source installer checks for Git, Rust, Cargo, and a C compiler before downloading anything. It clones the Spar compiler, language server, shell, their local Rust dependencies, and `oraclevs/spar-libraries`. It builds `spar`, `spar-ls`, `sparsh`, and the TCP native module for the host, then installs the binaries, standard library sources, and package sources.

```sh
curl -fsSLo install-spar-beta.sh https://raw.githubusercontent.com/oraclevs/sparsh/beta/install-from-source.sh
bash install-spar-beta.sh
```

The default binary directory is `~/.local/bin`. Set `SPAR_BIN_DIR` to choose another directory. Standard library and package sources go under `$SPA_HOME` when it is set, or `$XDG_DATA_HOME/spar` (`~/.local/share/spar` by default). The installer backs up a previous `stdlib/` or `libraries/` directory before replacing it.

The installer uses the `beta` branches of Spar, Sparsh, spar-ls, and spar-process; it uses `main` for the other source repositories. Set `SPAR_SOURCE_REF`, `SPARSH_SOURCE_REF`, `SPAR_LS_SOURCE_REF`, `SPAR_PROCESS_SOURCE_REF`, or `SPAR_LIBRARIES_SOURCE_REF` to test another revision. The matching standard library check stops installation if Spar and spar-libraries disagree.

Package sources are available locally under the Spar data directory. Spar's package manager still owns project dependencies and its global store. To use a package from GitHub, run `spar add` in a project, for example:

```sh
spar add args github:oraclevs/spar-libraries#spar-args
spar install
```

The GitHub `spar-tcp` branch contains source only. The beta installer builds its native module in the installed library copy; add TCP from that local path when you need it (for example, `spar add tcp "path:$HOME/.local/share/spar/libraries/spar-tcp"` with the default data directory).

The source installer needs Rust and a C compiler during the beta. It currently targets Unix hosts supported by the TCP build script. A separate prebuilt archive is available for Linux x86_64 GNU; its installer requires no compiler.

## Prebuilt archive

Run `bash build-toolchain.sh` from the Sparsh repository to build a target-named archive and SHA-256 sidecar under `dist/`. The archive contains the three binaries and matching standard library sources.

```sh
bash install-beta.sh dist/spar-toolchain-<version>-sparsh-<version>-<target>.tar.gz
```

For a hosted archive, pass its URL and published SHA-256 digest:

```sh
bash install-beta.sh https://example.org/spar-toolchain.tar.gz <sha256>
```

The prebuilt archive does not include the six separate packages. Those can be installed with Spar's existing package commands.
