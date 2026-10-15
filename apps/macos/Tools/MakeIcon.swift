import AppKit

let output = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)

func drawIcon(size: Int) -> Data? {
    guard let representation = NSBitmapImageRep(bitmapDataPlanes: nil,
        pixelsWide: size, pixelsHigh: size, bitsPerSample: 8, samplesPerPixel: 4,
        hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
        let graphics = NSGraphicsContext(bitmapImageRep: representation) else { return nil }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = graphics
    let context = graphics.cgContext
    context.scaleBy(x: CGFloat(size) / 1024, y: CGFloat(size) / 1024)
    let shell = NSBezierPath(roundedRect: NSRect(x: 40, y: 40, width: 944, height: 944), xRadius: 212, yRadius: 212)
    NSColor(calibratedRed: 0.065, green: 0.075, blue: 0.09, alpha: 1).setFill()
    shell.fill()
    NSColor(calibratedWhite: 1, alpha: 0.065).setStroke()
    shell.lineWidth = 3
    shell.stroke()
    for (index, y) in [320.0, 425.0, 530.0].enumerated() {
        let path = NSBezierPath()
        path.move(to: NSPoint(x: 250, y: y + 30))
        path.line(to: NSPoint(x: 512, y: y - 108))
        path.line(to: NSPoint(x: 774, y: y + 30))
        path.line(to: NSPoint(x: 512, y: y + 168))
        path.close()
        let color = index == 2
            ? NSColor(calibratedRed: 0.76, green: 0.92, blue: 0.53, alpha: 1)
            : NSColor(calibratedRed: 0.23 + Double(index) * 0.12, green: 0.31 + Double(index) * 0.12, blue: 0.18 + Double(index) * 0.08, alpha: 1)
        color.setFill()
        path.fill()
        NSColor(calibratedRed: 0.065, green: 0.075, blue: 0.09, alpha: 1).setStroke()
        path.lineWidth = 24
        path.lineJoinStyle = .round
        path.stroke()
    }
    NSGraphicsContext.restoreGraphicsState()
    return representation.representation(using: .png, properties: [:])
}

for pointSize in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let suffix = scale == 2 ? "@2x" : ""
        if let data = drawIcon(size: pointSize * scale) {
            try data.write(to: output.appendingPathComponent("icon_\(pointSize)x\(pointSize)\(suffix).png"))
        }
    }
}
