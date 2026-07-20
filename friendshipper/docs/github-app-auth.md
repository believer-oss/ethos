# GitHub App Auth

Friendshipper can mint GitHub credentials automatically through a GitHub App
instead of requiring users to create and paste a Personal Access Token. Users
click "Connect GitHub" once, authorize in the browser, and Friendshipper
manages short-lived tokens from then on: silent refresh, keyring storage, and
a git credential helper that replaces Git Credential Manager for the repo.

## How it works

1. The user clicks Connect GitHub (Preferences). The Friendshipper backend
   fetches the app's client id from friendshipper-server, generates a state
   nonce, and opens the browser to GitHub's authorize page.
2. GitHub redirects to `http://127.0.0.1:<port>/auth/github/callback` on the
   local backend, which forwards the authorization code (plus the user's Okta
   access token) to friendshipper-server.
3. friendshipper-server holds the app client secret and performs the
   code-for-token exchange. Only Okta-authenticated users can reach it.
4. The backend stores the token set in the OS keyring
   (`Friendshipper` / `github_oauth_tokens`) and points the GraphQL client,
   LFS lock calls, and submit flow at the access token.
5. `friendshipper-credential-helper` is installed as the repo-local
   `credential.helper`, so git and git-lfs read the same token from the
   keyring. No GCM popups.
6. Access tokens live 8 hours. The frontend piggybacks a refresh check on
   every Okta token event; the backend refreshes through
   friendshipper-server when the token is within an hour of expiry.

The legacy PAT path still works: the credential helper and all consumers fall
back to the `github_pat` keyring entry when no OAuth tokens exist.

## One-time setup (org admin)

Create a GitHub App on the org (Settings -> Developer settings -> GitHub
Apps):

- **Callback URLs**: register `http://127.0.0.1:8484/auth/github/callback`
  through `http://127.0.0.1:8494/auth/github/callback` (the backend probes
  ports 8484-8494).
- **Permissions**: Repository -> Contents (read/write), Pull requests
  (read/write), Metadata (read, mandatory).
- **Token expiration**: leave "Expire user authorization tokens" enabled.
  The flow requires refresh tokens.
- **Request user authorization (OAuth) during installation**: not required.
- **Webhook**: disable.
- Install the app on the org, limited to the repos Friendshipper manages.

Then configure friendshipper-server:

```
GITHUB_APP_CLIENT_ID=<app client id>
GITHUB_APP_CLIENT_SECRET=<app client secret>
```

Both must be set together. When absent, the `/github` routes are disabled
and clients keep using PATs.

## Local development

Run friendshipper-server locally with the env vars above (a test app on a
personal org works fine; register `http://127.0.0.1:8484/auth/github/callback`
as the callback). The credential helper builds as part of the workspace and
is picked up automatically when it sits next to `friendshipper.exe`, which is
where cargo puts both in `target/debug`.

To verify the helper end to end:

```
printf "protocol=https\nhost=github.com\n\n" | friendshipper-credential-helper get
```

It prints `username=x-access-token` and the current token when one is stored,
and nothing otherwise (git then falls through to the next helper).
