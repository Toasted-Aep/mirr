Set WshShell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
strDir = fso.GetParentFolderName(WScript.ScriptFullName)
strBat = Chr(34) & strDir & "\Start Mirr.bat" & Chr(34)
WshShell.Run strBat, 0, False
