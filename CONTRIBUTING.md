# Contributing to Prism

First off, thanks for taking the time to contribute!

All types of contributions are encouraged and valued. See the Table of Contents for different ways to help and details about how this project handles them. Please make sure to read the relevant section before making your contribution.

## Table of Contents
- [Contributing to Prism](#contributing-to-prism)
  - [Table of Contents](#table-of-contents)
  - [I Have a Question](#i-have-a-question)
  - [I Want To Contribute](#i-want-to-contribute)
    - [Legal Notice](#legal-notice)
    - [Reporting Bugs](#reporting-bugs)
    - [Suggesting Enhancements](#suggesting-enhancements)
    - [Your First Code Contribution](#your-first-code-contribution)
  - [Styleguides](#styleguides)
  - [Commit Messages](#commit-messages)

## I Have a Question
If you want to ask a question, we assume that you have read the available Documentation.

Before you ask a question, it is best to search for existing Issues that might help you. If you find a relevant issue that already exists and still need clarification, please add your question to that existing issue.

## I Want To Contribute

### Legal Notice
When contributing to this project, you must agree that you have authored 100% of the content, that you have the necessary rights to the content and that the content you contribute may be provided under the project license (AGPL-3.0). You will be required to sign a CLA (Contributor License Agreement).

### Reporting Bugs
**Before Submitting a Bug Report**
1. Make sure that you are using the latest version.
2. Determine if your bug is really a bug and not an error on your side.
3. Check if there is not already a bug report existing for your bug.

**How Do I Submit a Good Bug Report?**
You must never report security related issues, vulnerabilities or bugs including sensitive information to the issue tracker. Instead sensitive bugs must be sent by email to [INSERT EMAIL].

Open a GitHub Issue and explain:
- The behavior you would expect and the actual behavior.
- Reproduction steps.
- OS, Platform, and Version.

### Suggesting Enhancements
Enhancement suggestions are tracked as GitHub issues.
- Use a clear and descriptive title.
- Provide a step-by-step description of the suggested enhancement.
- Explain why this enhancement would be useful to most Prism users.

### Your First Code Contribution
**Pre-requisites**
You should first fork the repository and then clone your forked repository:
```bash
git clone https://github.com/abahjat/prism.git
```

Create a branch:
```bash
git checkout -b feature/my-cool-feature
```

**Development**
We use standard Rust tooling:
```bash
# Run tests
cargo test

# Check formatting
cargo fmt -- --check

# Check lints
cargo clippy
```

**Workflow**
1. Make your changes.
2. Add tests if applicable.
3. Ensure `cargo test` passes.
4. Ensure `cargo fmt` passes.
5. Push to your fork and submit a Pull Request.

## Styleguides
We strictly follow standard Rust formatting.
- Run `cargo fmt` before committing.
- Follow `clippy` suggestions where reasonable.

## Commit Messages
Please use conventional commits (e.g., `feat: add pptx parser`, `fix: resolve crash in pdf`).
