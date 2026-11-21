import Cocoa
import WebKit

class DevDriveAppDelegate: NSObject, NSApplicationDelegate, WKNavigationDelegate, WKUIDelegate {
    var window: NSWindow!
    var webView: WKWebView!
    var backendProcess: Process?
    let targetPort = 4040
    let appTitle = "DevDrive"

    func applicationDidFinishLaunching(_ notification: Notification) {
        setupMainMenu()

        let screenSize = NSScreen.main?.visibleFrame.size ?? CGSize(width: 1200, height: 800)
        let width = min(1200, screenSize.width * 0.9)
        let height = min(820, screenSize.height * 0.9)
        let rect = NSRect(x: 0, y: 0, width: width, height: height)

        window = NSWindow(
            contentRect: rect,
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = appTitle
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.backgroundColor = NSColor(red: 13/255.0, green: 17/255.0, blue: 23/255.0, alpha: 1.0)
        window.minSize = NSSize(width: 900, height: 620)
        window.center()
        window.isReleasedWhenClosed = false

        let config = WKWebViewConfiguration()
        config.preferences.setValue(true, forKey: "developerExtrasEnabled")

        webView = WKWebView(frame: window.contentView!.bounds, configuration: config)
        webView.autoresizingMask = [.width, .height]
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.setValue(false, forKey: "drawsBackground")

        window.contentView?.addSubview(webView)
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)

        ensureBackendAndLoad()
    }

    func setupMainMenu() {
        let mainMenu = NSMenu()

        // App Menu
        let appMenuItem = NSMenuItem()
        let appMenu = NSMenu()
        appMenu.addItem(NSMenuItem(title: "DevDrive Hakkında", action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)), keyEquivalent: ""))
        appMenu.addItem(NSMenuItem.separator())
        appMenu.addItem(NSMenuItem(title: "DevDrive'ı Gizle", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h"))
        let hideOthers = NSMenuItem(title: "Diğerlerini Gizle", action: #selector(NSApplication.hideOtherApplications(_:)), keyEquivalent: "h")
        hideOthers.keyEquivalentModifierMask = [.command, .option]
        appMenu.addItem(hideOthers)
        appMenu.addItem(NSMenuItem(title: "Tümünü Göster", action: #selector(NSApplication.unhideAllApplications(_:)), keyEquivalent: ""))
        appMenu.addItem(NSMenuItem.separator())
        appMenu.addItem(NSMenuItem(title: "DevDrive'dan Çık", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
        appMenuItem.submenu = appMenu
        mainMenu.addItem(appMenuItem)

        // Edit Menu
        let editMenuItem = NSMenuItem()
        let editMenu = NSMenu(title: "Düzen")
        editMenu.addItem(NSMenuItem(title: "Geri Al", action: #selector(UndoManager.undo), keyEquivalent: "z"))
        editMenu.addItem(NSMenuItem(title: "Yinele", action: #selector(UndoManager.redo), keyEquivalent: "Z"))
        editMenu.addItem(NSMenuItem.separator())
        editMenu.addItem(NSMenuItem(title: "Kes", action: #selector(NSText.cut(_:)), keyEquivalent: "x"))
        editMenu.addItem(NSMenuItem(title: "Kopyala", action: #selector(NSText.copy(_:)), keyEquivalent: "c"))
        editMenu.addItem(NSMenuItem(title: "Yapıştır", action: #selector(NSText.paste(_:)), keyEquivalent: "v"))
        editMenu.addItem(NSMenuItem(title: "Tümünü Seç", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a"))
        editMenuItem.submenu = editMenu
        mainMenu.addItem(editMenuItem)

        // View Menu
        let viewMenuItem = NSMenuItem()
        let viewMenu = NSMenu(title: "Görünüm")
        viewMenu.addItem(NSMenuItem(title: "Yenile", action: #selector(reloadDashboard), keyEquivalent: "r"))
        viewMenuItem.submenu = viewMenu
        mainMenu.addItem(viewMenuItem)

        // Window Menu
        let windowMenuItem = NSMenuItem()
        let windowMenu = NSMenu(title: "Pencere")
        windowMenu.addItem(NSMenuItem(title: "Simge Durumuna Küçült", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m"))
        windowMenu.addItem(NSMenuItem(title: "Kapat", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w"))
        windowMenuItem.submenu = windowMenu
        mainMenu.addItem(windowMenuItem)

        NSApp.mainMenu = mainMenu
    }

    @objc func reloadDashboard() {
        webView.reload()
    }

    func isBackendRunning() -> Bool {
        guard let url = URL(string: "http://127.0.0.1:\(targetPort)/api/status") else { return false }
        var request = URLRequest(url: url)
        request.timeoutInterval = 0.8
        let semaphore = DispatchSemaphore(value: 0)
        var running = false

        let task = URLSession.shared.dataTask(with: request) { _, response, _ in
            if let http = response as? HTTPURLResponse, http.statusCode == 200 {
                running = true
            }
            semaphore.signal()
        }
        task.resume()
        _ = semaphore.wait(timeout: .now() + 1.0)
        return running
    }

    func ensureBackendAndLoad() {
        if isBackendRunning() {
            loadURL()
            return
        }

        startBackendServer()

        DispatchQueue.global(qos: .userInitiated).async {
            var attempts = 0
            while attempts < 30 {
                if self.isBackendRunning() {
                    DispatchQueue.main.async {
                        self.loadURL()
                    }
                    return
                }
                Thread.sleep(forTimeInterval: 0.2)
                attempts += 1
            }
            DispatchQueue.main.async {
                self.loadURL()
            }
        }
    }

    func findDevDriveBinary() -> String? {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let possiblePaths = [
            Bundle.main.bundlePath + "/Contents/MacOS/devdrive-core",
            "\(home)/.cargo/bin/devdrive",
            "/usr/local/bin/devdrive"
        ]
        for path in possiblePaths {
            if FileManager.default.fileExists(atPath: path) {
                return path
            }
        }
        return nil
    }

    func startBackendServer() {
        guard let binPath = findDevDriveBinary() else {
            print("devdrive binary not found")
            return
        }
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: binPath)
        proc.arguments = ["ui", "--port", "\(targetPort)", "--no-open"]
        proc.environment = ProcessInfo.processInfo.environment

        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = pipe

        do {
            try proc.run()
            self.backendProcess = proc
        } catch {
            print("Failed to run backend: \(error)")
        }
    }

    func loadURL() {
        if let url = URL(string: "http://127.0.0.1:\(targetPort)") {
            webView.load(URLRequest(url: url))
        }
    }

    // Handle JavaScript Alert
    func webView(_ webView: WKWebView, runJavaScriptAlertPanelWithMessage message: String, initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping () -> Void) {
        let alert = NSAlert()
        alert.messageText = "DevDrive"
        alert.informativeText = message
        alert.alertStyle = .informational
        alert.addButton(withTitle: "Tamam")
        alert.runModal()
        completionHandler()
    }

    // Handle JavaScript Confirm
    func webView(_ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String, initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping (Bool) -> Void) {
        let alert = NSAlert()
        alert.messageText = "DevDrive"
        alert.informativeText = message
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Evet")
        alert.addButton(withTitle: "Vazgeç")
        let response = alert.runModal()
        completionHandler(response == .alertFirstButtonReturn)
    }

    // Open external links in default browser
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration, for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        if navigationAction.targetFrame == nil {
            if let url = navigationAction.request.url {
                NSWorkspace.shared.open(url)
            }
        }
        return nil
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        return true
    }

    func applicationWillTerminate(_ notification: Notification) {
        if let proc = backendProcess, proc.isRunning {
            proc.terminate()
        }
    }
}

let app = NSApplication.shared
let delegate = DevDriveAppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
