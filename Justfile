# Use PowerShell on Windows
set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

[unix]
pre-commit:
    @if command -v prek > /dev/null 2>&1; then prek run --all-files; else pre-commit run --all-files; fi

[windows]
pre-commit:
    if (Get-Command prek -ErrorAction SilentlyContinue) { prek run --all-files } else { pre-commit run --all-files }
