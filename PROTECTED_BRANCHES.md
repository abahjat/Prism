# Protecting the Main Branch

To ensure stability and enforce the AGPL-3.0/Commercial dual license compliance (e.g., checking for header compliance or verified contributors), you should enforce protection rules on the `main` branch.

## Recommended Settings

1.  **Require a pull request before merging**
    *   [x] Require approvals (Minimum 1)
    *   [x] Dismiss stale pull request approvals when new commits are pushed
    *   [x] Require review from Code Owners

2.  **Require status checks to pass before merging**
    *   [x] Require branches to be up to date before merging
    *   `ci` (GitHub Actions workflow) must pass.

3.  **Include administrators**
    *   [x] Enforce all configured restrictions for administrators.

## How to Configure
1.  Go to your GitHub Repository > **Settings**.
2.  Click **Branches** in the sidebar.
3.  Click **Add rule**.
4.  Branch name pattern: `main`.
5.  Check the boxes listed above.
6.  Click **Create**.
