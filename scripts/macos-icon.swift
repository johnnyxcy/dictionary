// Draw the local app icon using native vector paths and system fonts.
import AppKit

let image = NSImage(size: NSSize(width: 1024, height: 1024))
image.lockFocus()
let tile = NSBezierPath(roundedRect: NSRect(x: 80, y: 80, width: 864, height: 864), xRadius: 192, yRadius: 192)
NSGradient(starting: NSColor(srgbRed: 0.15, green: 0.35, blue: 0.56, alpha: 1),
           ending: NSColor(srgbRed: 0.07, green: 0.20, blue: 0.35, alpha: 1))!.draw(in: tile, angle: -90)
let book = NSBezierPath(roundedRect: NSRect(x: 212, y: 246, width: 600, height: 532), xRadius: 46, yRadius: 46)
NSColor(srgbRed: 0.97, green: 0.95, blue: 0.90, alpha: 1).setFill()
book.fill()
let fold = NSBezierPath()
fold.move(to: NSPoint(x: 512, y: 284))
fold.line(to: NSPoint(x: 512, y: 740))
fold.lineWidth = 5
NSColor(srgbRed: 0.76, green: 0.78, blue: 0.77, alpha: 1).setStroke()
fold.stroke()
let ink = NSColor(srgbRed: 0.11, green: 0.27, blue: 0.43, alpha: 1)
let style = NSMutableParagraphStyle()
style.alignment = .center
for (text, x, font) in [("A", 230.0, NSFont.systemFont(ofSize: 204, weight: .medium)),
                        ("文", 530.0, NSFont.systemFont(ofSize: 184, weight: .medium))] {
    (text as NSString).draw(in: NSRect(x: x, y: 382, width: 264, height: 256),
                           withAttributes: [.font: font, .foregroundColor: ink, .paragraphStyle: style])
}
image.unlockFocus()
let bitmap = NSBitmapImageRep(data: image.tiffRepresentation!)!
try bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
