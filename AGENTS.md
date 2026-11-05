# Repository Rules

## Text Editing and Change Visibility

- Use `apply_patch` for ordinary text and source-code edits. Preserve the original encoding and line endings. If `apply_patch` cannot safely preserve them, use a narrowly scoped alternative and explain why.
- If no change card is visible, identify the modified files and summarize the changes in the final response.

## Temporary Files

- Keep temporary AI scripts, probes, screenshots, and verification output outside the repository, under a task-specific directory in the system temporary directory.
- Exclude `AI_Temp/` and temporary artifacts from staging, commits, and submission lists.

## File Deletion

- When deleting files or directories with PowerShell, do not pass `-Force` to `Remove-Item`.
