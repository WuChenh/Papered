#Requires -RunAsAdministrator

nssm install PaperedDaemon "C:\path\to\papered-daemon.exe"
nssm set PaperedDaemon AppDirectory "C:\path\to\app"
nssm set PaperedDaemon AppStdout "C:\Users\<you>\AppData\Local\papered\daemon.log"
nssm set PaperedDaemon AppStderr "C:\Users\<you>\AppData\Local\papered\daemon.log"
nssm set PaperedDaemon AppEnvironmentExtra RUST_LOG=info
nssm start PaperedDaemon
