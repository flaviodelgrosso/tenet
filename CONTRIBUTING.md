# Contributing to tenet

Thank you for helping improve `tenet`. Contributions that test its assumptions are especially valuable: adversarial contract cases, deterministic verification, evidence validity, exact authority/candidate snapshot binding, documentation, and negative results.

Please read the [Code of Conduct](CODE_OF_CONDUCT.md) before participating.

## Before you start

- Search existing issues and pull requests before opening a new one.
- For a substantial change, open an issue first so the scope and design can be discussed.
- Keep changes focused. Separate unrelated fixes into separate pull requests.
- Do not include secrets, credentials, generated local audit state, or unrelated `.tenet/` artifacts in commits.

## Development setup

`tenet` is a Rust project. Install a current stable Rust toolchain, then clone the repository and work from the checkout:

```bash
git clone https://github.com/flaviodelgrosso/tenet.git
cd tenet
```

The repository provides Make targets for the standard checks:

```bash
make check
make test
make clippy
```

Run the complete CI-equivalent quality gate before submitting a pull request:

```bash
make ci
```

You can install the local binary while testing CLI changes with:

```bash
make install
```

If you change Rust source, run `make fmt` and include only the formatting changes relevant to your work.

## Making a change

1. Create a branch from the current default branch.
2. Read the relevant code, tests, README sections, and completion-authority requirements before changing behavior.
3. Preserve deterministic completion derivation, explicit authority/candidate snapshot binding, authority-surface immutability, and fail-closed evidence semantics.
4. Add or update tests when a change introduces or alters observable behavior.
5. Update documentation and examples when commands, configuration, or user-visible behavior changes.
6. Run the applicable checks, preferably `make ci`.
7. Review the final diff for accidental files, credentials, generated state, and unrelated formatting.

## Pull requests

A useful pull request description includes:

- the problem and motivation;
- the approach and important tradeoffs;
- user-visible behavior or compatibility effects;
- tests and commands run, including any checks you could not run;
- relevant issue links or reproduction steps.

Pull requests should be reviewable, scoped to one concern, and ready for discussion. Review feedback is part of the process; please respond to it or explain why a different approach is safer.

## Reporting bugs and proposing ideas

Open an issue at <https://github.com/flaviodelgrosso/tenet/issues> with a clear title and enough detail to reproduce or evaluate the report. For bugs, include:

- the `tenet` version and Rust toolchain;
- operating system and relevant backend versions;
- the command and configuration used;
- expected and actual behavior;
- minimal reproduction steps;
- logs or `.tenet/` evidence with secrets removed.

For security vulnerabilities or reports involving harassment, do not use a public issue. Contact the repository maintainers privately through GitHub and avoid disclosing the issue publicly until a fix or disclosure plan is agreed.

## License

By contributing, you agree that your contributions are provided under the project's [MIT License](LICENSE).
