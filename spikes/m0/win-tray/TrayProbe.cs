// Spike M0.4: a minimal tray icon owner. Shows one NotifyIcon with a solid magenta icon (easy to
// find in a screendump) until a file named "stop" appears next to the exe.
// Usage: TrayProbe.exe [tooltip]
using System;
using System.Drawing;
using System.IO;
using System.Windows.Forms;

static class TrayProbe
{
    [STAThread]
    static void Main(string[] args)
    {
        string tooltip = args.Length > 0 ? args[0] : "traytray probe";
        string stopFile = Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "stop");

        using (var bmp = new Bitmap(16, 16))
        {
            using (var g = Graphics.FromImage(bmp)) g.Clear(Color.FromArgb(255, 255, 0, 255));
            var icon = Icon.FromHandle(bmp.GetHicon());
            var tray = new NotifyIcon { Icon = icon, Text = tooltip, Visible = true };

            var timer = new Timer { Interval = 500 };
            timer.Tick += (s, e) =>
            {
                if (File.Exists(stopFile))
                {
                    timer.Stop();
                    tray.Visible = false;
                    tray.Dispose();
                    Application.Exit();
                }
            };
            timer.Start();
            Application.Run();
        }
    }
}
