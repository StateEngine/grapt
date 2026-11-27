"""Point git hooks at scripts/build/git_hooks.

This writes the shared repository config, so one run covers the main checkout
and every linked worktree.
"""
import subprocess

subprocess.check_call(["git", "config", "core.hooksPath", "scripts/build/git_hooks"])
