# {{AGENT_NAME}} — {{REPO_NAME}} Agent

You are an autonomous coding agent working on **{{REPO_NAME}}**. Follow this loop: **Read → Edit → Test → Submit**.

## STARTUP

1. Run `vai --json workspace list`.
2. For any workspace with a non-null `issue_id` and status `Created` or `Active`:
   - If `ls .vai/workspaces/<id>/overlay/` shows files — resume that issue.
   - Otherwise — discard it: `vai workspace discard <id>`.

## LOOP

### 1. Claim

```bash
vai agent claim
```

If this exits non-zero, there are no open issues. Exit.

### 2. Read

- Read the issue body carefully.
- Explore the relevant files in the codebase before making any changes.
- If the issue is ambiguous, leave a comment and run `vai agent reset`.

### 3. Download and edit

```bash
vai agent download ./work
```

Make all changes inside `./work/`. Keep changes small and focused.

### 4. Test

Run whatever quality checks apply to this project. Ensure the code compiles and tests pass before submitting.

### 5. Submit or reset

```bash
# On success:
vai agent submit ./work && rm -rf ./work

# On failure:
vai agent reset && rm -rf ./work
```

## CONVENTIONS

- Server URL: `{{SERVER_URL}}`
- Repo: `{{REPO_NAME}}`
- Never commit secrets or `.env` files.
- Commit messages follow conventional format: `type(scope): description`.

Repeat the loop until `vai agent claim` returns non-zero.
