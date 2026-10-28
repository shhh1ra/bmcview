// Печатает байткод одного метода из класса (jjs dump-method.js -- <class-file> <methodName>)
var Files = Java.type("java.nio.file.Files");
var Paths = Java.type("java.nio.file.Paths");
var ClassReader = Java.type("jdk.internal.org.objectweb.asm.ClassReader");
var Textifier = Java.type("jdk.internal.org.objectweb.asm.util.Textifier");
var TraceClassVisitor = Java.type("jdk.internal.org.objectweb.asm.util.TraceClassVisitor");
var StringWriter = Java.type("java.io.StringWriter");
var PrintWriter = Java.type("java.io.PrintWriter");
var bytes = Files.readAllBytes(Paths.get(arguments[0]));
var sw = new StringWriter();
new ClassReader(bytes).accept(new TraceClassVisitor(null, new Textifier(), new PrintWriter(sw)), 0);
var text = String(sw.toString());
var start = text.indexOf(" " + arguments[1] + "(");
var end = text.indexOf("\n\n", start);
print(text.substring(start, end));
