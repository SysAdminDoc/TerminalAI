using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Forms;

internal static class TerminalAiWdioHost
{
    private const uint SwpNoActivate = 0x0010;
    private const uint SwpNoZOrder = 0x0004;
    private const uint SwpShowWindow = 0x0040;
    private static readonly IntPtr DpiAwarenessContextPerMonitorV2 = new IntPtr(-4);

    private delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);

    [DllImport("user32.dll")]
    private static extern bool SetProcessDpiAwarenessContext(IntPtr value);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool SetWindowPos(
        IntPtr hWnd,
        IntPtr hWndInsertAfter,
        int x,
        int y,
        int width,
        int height,
        uint flags);

    private static void PlaceTerminalWindows(
        int displayX,
        int displayY,
        int displayWidth,
        int displayHeight,
        string expectedBinary,
        string placementPath)
    {
        var width = Math.Max(640, Math.Min(displayWidth - 40, 1440));
        var height = Math.Max(480, Math.Min(displayHeight - 40, 900));
        EnumWindows((hWnd, _) =>
        {
            uint processId;
            GetWindowThreadProcessId(hWnd, out processId);
            if (processId == 0) return true;

            try
            {
                using (var process = Process.GetProcessById((int)processId))
                {
                    var binary = process.MainModule.FileName;
                    if (!String.Equals(binary, expectedBinary, StringComparison.OrdinalIgnoreCase)) return true;
                }

                if (!SetWindowPos(
                    hWnd,
                    IntPtr.Zero,
                    displayX + 20,
                    displayY + 20,
                    width,
                    height,
                    SwpNoActivate | SwpNoZOrder | SwpShowWindow)) return true;

                File.WriteAllText(
                    placementPath,
                    "{\"processId\":" + processId +
                    ",\"x\":" + (displayX + 20) +
                    ",\"y\":" + (displayY + 20) +
                    ",\"width\":" + width +
                    ",\"height\":" + height + "}");
            }
            catch (Exception)
            {
                // The process can exit between EnumWindows and Process lookup.
            }
            return true;
        }, IntPtr.Zero);
    }

    [STAThread]
    private static int Main()
    {
        try { SetProcessDpiAwarenessContext(DpiAwarenessContextPerMonitorV2); }
        catch (EntryPointNotFoundException) { }

        var runnerPath = Environment.GetEnvironmentVariable("TERMINALAI_E2E_RUNNER");
        var nodePath = Environment.GetEnvironmentVariable("TERMINALAI_E2E_NODE");
        var appBinary = Environment.GetEnvironmentVariable("TERMINALAI_E2E_APP_BINARY");
        var placementPath = Environment.GetEnvironmentVariable("TERMINALAI_E2E_PLACEMENT");
        var displayX = Environment.GetEnvironmentVariable("TERMINALAI_E2E_DISPLAY_X");
        var displayY = Environment.GetEnvironmentVariable("TERMINALAI_E2E_DISPLAY_Y");
        var displayWidth = Environment.GetEnvironmentVariable("TERMINALAI_E2E_DISPLAY_WIDTH");
        var displayHeight = Environment.GetEnvironmentVariable("TERMINALAI_E2E_DISPLAY_HEIGHT");
        int parsedX;
        int parsedY;
        int parsedWidth;
        int parsedHeight;
        if (String.IsNullOrWhiteSpace(runnerPath) || String.IsNullOrWhiteSpace(nodePath) ||
            String.IsNullOrWhiteSpace(appBinary) || String.IsNullOrWhiteSpace(placementPath) ||
            !Int32.TryParse(displayX, out parsedX) || !Int32.TryParse(displayY, out parsedY) ||
            !Int32.TryParse(displayWidth, out parsedWidth) || !Int32.TryParse(displayHeight, out parsedHeight)) return 2;

        appBinary = Path.GetFullPath(appBinary);
        var startInfo = new ProcessStartInfo
        {
            FileName = nodePath,
            Arguments = "\"" + runnerPath + "\"",
            WorkingDirectory = Directory.GetParent(Directory.GetParent(runnerPath).FullName).FullName,
            UseShellExecute = false,
            CreateNoWindow = true,
        };
        using (var runner = Process.Start(startInfo))
        using (var form = new Form())
        {
            if (runner == null) return 3;
            form.Text = "TerminalAI WebDriver isolation host";
            form.Width = 320;
            form.Height = 120;
            form.ShowInTaskbar = false;
            form.FormBorderStyle = FormBorderStyle.FixedToolWindow;
            form.StartPosition = FormStartPosition.Manual;
            form.Opacity = 0.01;
            runner.EnableRaisingEvents = true;
            runner.Exited += (_, __) =>
            {
                try { form.BeginInvoke(new Action(form.Close)); } catch (InvalidOperationException) { }
            };
            using (var placementTimer = new Timer { Interval = 25 })
            {
                placementTimer.Tick += (_, __) => PlaceTerminalWindows(
                    parsedX,
                    parsedY,
                    parsedWidth,
                    parsedHeight,
                    appBinary,
                    placementPath);
                placementTimer.Start();
                Application.Run(form);
                placementTimer.Stop();
            }
            runner.WaitForExit();
            return runner.ExitCode;
        }
    }
}
