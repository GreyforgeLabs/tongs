# Contributing to tongs

Thanks for your interest in contributing. This guide covers the process for submitting changes.

## Getting Started

1. Fork the repository
2. Clone your fork: `git clone https://github.com/YOUR_USERNAME/tongs.git`
3. Run setup: `cd tongs && ./scripts/setup.sh`
4. Create a branch: `git checkout -b your-feature`

## Development Workflow

1. Make your changes
2. Run `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings` and `cargo test --locked`
3. Commit with clear, descriptive messages (prefer `feat:`, `fix:`, `docs:` prefixes)
4. Push to your fork and open a Pull Request

## Commit Messages

Use [Conventional Commits](https://www.conventionalcommits.org/) style:

```
feat: add new parsing mode
fix: handle empty input gracefully
docs: update installation steps
chore: update dependencies
```

## Pull Request Process

1. Fill out the PR template
2. Ensure CI passes
3. One maintainer approval is required for merge
4. Keep PRs focused — one logical change per PR

## Code Standards

- Follow the existing code style in the repository (`cargo fmt`)
- Write tests for new functionality
- Keep the on-disk format (including the `"atomic-json-store/1"` envelope marker), JSON formatting and lock protocol compatible with the Python 1.x implementation (released as `atomic-json-store`); run `tests/differential.rs` against a v1.0.1 checkout when touching them (see STARTHERE.md)
- Update documentation for user-facing changes
- No secrets, credentials, or internal paths in your code

## Reporting Issues

Use the GitHub issue templates. For bugs, include steps to reproduce. For features, describe the problem you're solving.

## License

By contributing, you agree that your contributions will be licensed under the same license as this project (AGPL-3.0).

---

Built by [Greyforge](https://greyforge.tech)
