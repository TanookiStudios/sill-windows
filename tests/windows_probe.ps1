# A small window for tests/end_to_end.rs's input test: a text box and a panel. It reports where they
# are, then writes what really reached them, so the test checks Windows' own view of the input.
param([string]$Dir)
Add-Type -AssemblyName System.Windows.Forms,System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()
$f = New-Object System.Windows.Forms.Form
$f.Text = 'Sill input probe'; $f.StartPosition = 'Manual'; $f.Location = '60,60'; $f.Size = '520,420'; $f.TopMost = $true
$t = New-Object System.Windows.Forms.TextBox
$t.Location = '20,20'; $t.Size = '460,30'; $t.Font = New-Object System.Drawing.Font('Segoe UI', 14)
$p = New-Object System.Windows.Forms.Panel
$p.Location = '20,80'; $p.Size = '460,280'; $p.BackColor = 'LightBlue'
$f.Controls.Add($t); $f.Controls.Add($p)
$log = Join-Path $Dir 'events.txt'
$note = { param($s) Add-Content -Path $log -Value $s -Encoding UTF8 }
# A classic menu bar (a real HMENU), for the menus test.
$mm = New-Object System.Windows.Forms.MainMenu
$file = $mm.MenuItems.Add('&File')
$save = New-Object System.Windows.Forms.MenuItem('&Save', [EventHandler]{ & $note 'menu save' }, [System.Windows.Forms.Shortcut]::CtrlS)
$file.MenuItems.Add($save) | Out-Null
$file.MenuItems.Add('-') | Out-Null
$print = $file.MenuItems.Add('&Print...'); $print.Enabled = $false
$auto = $file.MenuItems.Add('&Autosave'); $auto.Checked = $true
$f.Menu = $mm
$t.Add_TextChanged({ & $note ("text " + [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($t.Text))) })
$p.Add_MouseDown({ param($s, $e) & $note ("down " + $e.Button + " " + $e.X + " " + $e.Y) })
$p.Add_MouseEnter({ $p.Focus() | Out-Null })
$p.Add_MouseWheel({ param($s, $e) & $note ("wheel " + $e.Delta) })
$f.Add_Resize({ & $note ("state " + $f.WindowState) })
$f.Add_FormClosed({ & $note "closed" })
$f.Add_Shown({
  $f.Activate()
  $tb = $t.RectangleToScreen($t.ClientRectangle); $pb = $p.RectangleToScreen($p.ClientRectangle)
  $s = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
  $o = @{ screenW = $s.Width; screenH = $s.Height; textX = $tb.X + 40; textY = $tb.Y + 12; panelX = $pb.X + 100; panelY = $pb.Y + 100 }
  [IO.File]::WriteAllText((Join-Path $Dir 'where.json'), ($o | ConvertTo-Json))
})
[System.Windows.Forms.Application]::Run($f)
