$action = New-ScheduledTaskAction -Execute "C:\path\to\papered-daemon.exe"
$trigger = New-ScheduledTaskTrigger -AtLogOn
$settings = New-ScheduledTaskSettingsSet -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
Register-ScheduledTask -TaskName "PaperedDaemon" -Action $action -Trigger $trigger -Settings $settings -RunLevel Highest

Start-ScheduledTask -TaskName "PaperedDaemon"
# Verify
Get-ScheduledTask -TaskName "PaperedDaemon" | fl State,LastRunTime,LastTaskResult
