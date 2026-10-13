# Reporting a problem

Twin never sends anything on its own. There is no telemetry, no crash upload
and no account. To report a problem, you send the report yourself.

## Where to send it

Open an issue at the repository named in `Cargo.toml`
(`https://github.com/tpt-apps/tpt-app-digital-twin`). Do not put private twin
data in a public issue; see "Sharing a project" below.

## What to include

1. **Twin version.** The `version` in `Cargo.toml` of the build you ran.
2. **Operating system and build.** For example Windows 11, official or source
   build.
3. **What you did, step by step,** and what you expected to happen.
4. **What happened instead.** Copy the status line and any error text exactly.
5. **Whether it happens every time.**

## Sharing a project

A project is a folder. If the problem depends on your twin, you can export it
as a `.twin` package (Packages card, Export twin) and attach that file. Be
aware of what it holds:

- the twin's parameters, scenarios, design history, runs and sensor data;
- your export signing key's **public** part only. The private key stays in your
  settings folder (`%APPDATA%\twin\export-key.seed` on Windows).

Export with a passphrase if the file will travel further than the person you
send it to. Only share a package you are comfortable sharing.

## Logs

Twin does not write a log file yet. Include the exact text of any error shown
in the window; that is the most useful detail until logging exists.
