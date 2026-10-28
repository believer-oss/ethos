# Overview

Friendshipper is designed to be a one-stop-shop for all nontechnical users' source control needs. For Unreal projects,
it essentially replaces workflows that UGS and P4V support.

### Features

- Commit history
- Syncing repo + engine/editor binaries
- Submitting files + optional commit message formatting
- Reverting files
- LFS lock management
- Generating code project files + opening editor
- Automatic and manual snapshots of worktree state
- Background fetching of latest
- Background repo maintenance (git gc, etc)

### Repo Requirements

- Must be hosted on Github with merge queue enabled
- LFS server with locking support (see [lfs-rs](https://github.com/believer-oss/lfs-rs))

### Feature branches

- Create a branch from Branch Management with "New branch". The name becomes `f11r-branch-<slug>` and must be unique.
  New branches start from the latest `origin/main` (your repository's trunk branch) and are pushed to origin immediately.
- Switch branches with the branch selector on the submit page or on Branch Management. Switching requires no local
  changes and Unreal Editor closed. Binaries and the engine update as they do after a Sync.
- The reset button returns you to your configured target branch.
- By default the selector lists `f11r-branch-*` branches; turn on "Show all branches" in Preferences (Source Control
  Options) to list every branch on origin except Quick Submit branches.
- Quick Submit is available on your target branch and its Quick Submit branches. On another target branch, or on a Quick
  Submit branch made for another target, it is blocked. From a Quick Submit branch, Sync returns you to your target
  branch; the selector and reset button are disabled there.
- On any other branch, the submit page offers **Submit to branch** instead: it commits the selected files to the current
  branch and pushes them to origin, and your locks are kept. See [Submit to branch](#submit-to-branch).
- Branch Management (Git 2.38 or newer) shows how the current branch compares with the trunk: commits ahead and behind,
  sync with origin and predicted conflicts. Under **Changes on branch**, which covers pushed commits only, the Lockable
  files tab flags files also changed on the trunk ("will conflict at rebase"), and the Commits tab lists the branch's
  commits. **Create PR** opens a pull request into the trunk and shows it in your browser; it does not queue it. Once
  the branch has an open pull request, **Add to merge queue** queues it as soon as GitHub reports it mergeable, and a
  notification reports the result. Both are disabled while the branch is behind origin, has unpushed commits or is
  predicted to conflict. After the branch is merged or deleted, a banner offers "Return to <target branch>".
- Branches are deleted on GitHub, not in Friendshipper. **Clean up deleted branches** on Branch Management then removes
  your local copies of `f11r-branch-*` branches deleted on origin, after a preview that shows each branch's SHA and
  flags commits that are not on the trunk. **Clean up all merged** removes only merged branches; a branch with commits
  not on the trunk is removed only when you pick it and confirm.
- **Rebase onto latest main** on Branch Management (Git 2.43 or newer) replays the branch's pushed commits on top of
  the latest trunk. It needs a clean branch that is in sync with origin, and Unreal Editor closed. A backup is kept
  under `refs/f11r-backup` and pruned after 14 days. After you review the result, **Push rebased branch** force-pushes
  it as a separate step; collaborators must Sync afterwards. **Reset to remote version** instead discards the rebase
  and puts the branch back the way it was on origin when the rebase started.
- Branch Management is disabled while Unreal Editor has the project open.
- Conflicts are resolved one commit at a time by keeping main's or the branch's version of each file; text files can
  also be merged in an editor, except files Windows would run. A choice can be undone until you confirm the commit.
  Continue is refused while edits are unsaved, files you staged yourself are present or conflict markers remain. Abort
  returns the branch to where it started.
- While a rebase is in progress or waiting to be pushed, Sync, submit and other changes are blocked; locking still works.
- If someone pushes to the branch during the rebase, choose **Redo rebase on new origin**, or **Reset to backup** if
  origin's branch was rewritten. Branches that change C++ get an offer to trigger an editor build afterwards.

### Limitations

- Assumes all development happens on a single trunk branch
- Only supports a single project at a time

# Setup

You will need:

- Github repo
- Personal Access Token

If using the editor/engine binary download features, you will also need:

- [AWS creds and setup](https://github.com/believer-oss/ethos/blob/rjd/readme-updates/friendshipper/docs/setup.md#aws)

Upon first launch of Friendshipper, you will be prompted to fill in this information. You can always update it in the Preferences page later.

# Quick Submit

One of the main drawbacks of Git in a large team environment is the requirement to be on
latest to submit changes. To overcome this limitation, Friendshipper submits changes through
an alternate submission path that leverages Github's
[Merge Queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue)
feature. This means that Friendshipper is tightly integrated with GitHub's feature set, but it
achieves parity with traditional Perforce workflows in the sense that users can submit files
at any given time, as long as it hasn't changed upstream and other users haven't locked it.

The way this works:

1. The user initiates a submit
2. A snapshot is saved in case anything goes wrong, the user won't lose any work
3. A new branch is created with all the changes
4. The changes are commited to the new branch
5. A temporary worktree branch is created, with all the same changes
6. Latest is fetched from upstream trunk and merged into the worktree branch
7. The worktree branch is pushed up to the remote
8. A new PR is opened to merge the change into the trunk branch
9. The PR is added to the merge queue
10. Once the PR passes all merge checks, it's merged into trunk

Because the Quick Submit process leaves the user on a leaf branch, Friendshipper uses the Sync
operation to switch the user back to the trunk branch if they're not on it already.

Quick Submit always sends to the target branch set in Preferences. The submit button and the colored
mode badge (one color per target branch) read "Quick Submit to <target>". The preview names the target and says
whether it merges via the merge queue or directly. If you're on a different target branch (for example on `main`
while the target is `content-main`), Quick Submit is blocked and a red banner offers "Switch to <target>" (or
"Sync to <target>" when you're on a Quick Submit branch) and "Open Preferences".

It should be noted that this process is mainly intended to be used by nontechnical users, or for
content-only submits. Engineers may want to have more control over their submission process and
are empowered to do so.

## Submit to branch

On a branch that is neither a target branch nor a Quick Submit branch, the submit page offers **Submit to branch**.
A banner appears when the branch was not created by Friendshipper. Submit to branch commits the selected files, then
fast-forward pushes to `origin/<branch>`, but only if origin has not changed since the up-to-date check. It never
force-pushes, and a pre-submit snapshot is kept.

- **Locks:** they are kept, and freed when the branch merges to main. On a branch that never merges, release them by hand.
- **Behind origin:** Sync first. If someone pushed in the meantime, your commit is kept locally; Sync, then use "Push N commits".
- **Files changed on main:** they show a yellow "will conflict at rebase" badge and do not block the submit.
- **Detached HEAD or rebase in progress:** submitting is unavailable. Open Branch Management.

# [Unreal Plugin](https://github.com/believer-oss/FriendshipperSourceControl)

Unreal requires tight integration with whatever source control provider you're using for your
repo, so Friendshipper has a plugin to facilitate this. See the plugin page for installation
documentation.
