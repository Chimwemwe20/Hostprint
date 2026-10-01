<#
.SYNOPSIS
    Build, test and run Hostprint inside Docker, without a local Rust toolchain.

.DESCRIPTION
    Commands:
      check            fmt check, clippy and tests: what CI runs
      test [args]      cargo test --workspace [args]
      fmt              cargo fmt --all
      build            static release binary in dist/hostprint
      run [args]       run the CLI, e.g.  run capture --name healthy
      demo             build, then run examples/demo against your Docker
      shell            interactive shell in the toolchain container
      cargo [args]     any other cargo command
      clean            remove the cache volumes and the dev image

    The toolchain image (hostprint-dev) is built on first use. Cargo's
    registry, build output and the snapshots made with "run" persist in the
    Docker volumes hostprint-cargo, hostprint-target and hostprint-home.

.EXAMPLE
    ./scripts/dev.ps1 check

.EXAMPLE
    ./scripts/dev.ps1 run diff healthy
#>
# Plain $args rather than a param() block, so that arguments such as --name
# are passed through to hostprint instead of being parsed by PowerShell.
$Command = if ($args.Count -gt 0) { [string]$args[0] } else { "help" }
$Rest = if ($args.Count -gt 1) { [string[]]$args[1..($args.Count - 1)] } else { @() }

# Failures are detected through $LASTEXITCODE. ErrorActionPreference stays at
# its default because Windows PowerShell turns any stderr output of a native
# command into an error record, which "Stop" would make fatal.
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Image = "hostprint-dev"
$Volumes = @(
    "-v", "${Root}:/src",
    "-v", "hostprint-cargo:/usr/local/cargo/registry",
    "-v", "hostprint-target:/target"
)

function Assert-Docker {
    $null = docker info --format "{{.ServerVersion}}" 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "Docker is not running. Start Docker Desktop and try again." -ForegroundColor Red
        exit 1
    }
}

function Initialize-Image {
    $null = docker image inspect $Image 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "Building the $Image toolchain image (first run only)..."
        docker build -t $Image -f (Join-Path $Root "docker/dev.Dockerfile") (Join-Path $Root "docker")
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
}

# Runs a command in the toolchain container and exits on failure.
function Invoke-Dev {
    param([string[]] $CommandLine, [string[]] $Extra = @())
    $flags = @("run", "--rm") + $Volumes + $Extra
    # A TTY gives colored output, but fails when output is redirected.
    if (-not [Console]::IsOutputRedirected) { $flags += "-t" }
    docker @flags $Image @CommandLine
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

$DockerSocket = @("-v", "/var/run/docker.sock:/var/run/docker.sock")
# No double quotes inside: Windows PowerShell mangles them in native arguments.
$StaticBuild = 'set -e; t=$(uname -m)-unknown-linux-musl; cargo build --release --locked --target $t; mkdir -p /src/dist; cp /target/$t/release/hostprint /src/dist/hostprint; ls -l /src/dist/hostprint'

if ($Command -in @("help", "-h", "--help")) {
    Get-Help $PSCommandPath -Detailed
    exit 0
}

Assert-Docker
if ($Command -eq "clean") {
    $null = docker volume rm hostprint-cargo hostprint-target hostprint-home 2>&1
    $null = docker image rm $Image 2>&1
    Write-Host "Removed the hostprint-dev image and cache volumes."
    exit 0
}
Initialize-Image

switch ($Command) {
    "check" {
        Invoke-Dev @("sh", "-c", "cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace")
    }
    "test"  { Invoke-Dev (@("cargo", "test", "--workspace") + $Rest) }
    "fmt"   { Invoke-Dev @("cargo", "fmt", "--all") }
    "build" { Invoke-Dev @("sh", "-c", $StaticBuild) }
    "run" {
        Invoke-Dev (@("cargo", "run", "-q", "-p", "hostprint", "--") + $Rest) `
            -Extra ($DockerSocket + @("-v", "hostprint-home:/root/.hostprint"))
    }
    "demo" {
        Invoke-Dev @("sh", "-c", $StaticBuild)
        $flags = @("run", "--rm", "-v", "${Root}:/src") + $DockerSocket + @("-e", "HOSTPRINT=/src/dist/hostprint")
        docker @flags docker:cli sh -c "apk add -q git >/dev/null && sh /src/examples/demo/run-demo.sh"
        exit $LASTEXITCODE
    }
    "shell" {
        $flags = @("run", "--rm", "-it") + $Volumes + $DockerSocket + @("-v", "hostprint-home:/root/.hostprint")
        docker @flags $Image bash
    }
    "cargo" { Invoke-Dev (@("cargo") + $Rest) }
    default {
        Write-Host "Unknown command '$Command'. Run ./scripts/dev.ps1 help" -ForegroundColor Red
        exit 2
    }
}
