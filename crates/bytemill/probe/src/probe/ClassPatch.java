package probe;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;

/** Replaces one method body with {@code return}. No library, Java 8 bytecode. */
public final class ClassPatch {
    private ClassPatch() {}

    public static byte[] nopMethod(byte[] in, String method, String desc) {
        try {
            return rewrite(in, method, desc);
        } catch (IOException e) {
            throw new IllegalStateException(e);
        }
    }

    private static byte[] rewrite(byte[] in, String method, String desc) throws IOException {
        DataInputStream d = new DataInputStream(new ByteArrayInputStream(in));
        ByteArrayOutputStream raw = new ByteArrayOutputStream(in.length);
        DataOutputStream o = new DataOutputStream(raw);
        if (d.readInt() != 0xCAFEBABE) {
            throw new IOException("not a class file");
        }
        o.writeInt(0xCAFEBABE);
        o.writeShort(d.readUnsignedShort());
        o.writeShort(d.readUnsignedShort());
        int cpCount = d.readUnsignedShort();
        o.writeShort(cpCount);
        String[] utf = new String[cpCount];
        for (int i = 1; i < cpCount; i++) {
            int tag = d.readUnsignedByte();
            o.writeByte(tag);
            switch (tag) {
                case 1:
                    String s = d.readUTF();
                    utf[i] = s;
                    o.writeUTF(s);
                    break;
                case 7:
                case 8:
                case 16:
                case 19:
                case 20:
                    o.writeShort(d.readUnsignedShort());
                    break;
                case 15:
                    o.writeByte(d.readUnsignedByte());
                    o.writeShort(d.readUnsignedShort());
                    break;
                case 3:
                case 4:
                case 9:
                case 10:
                case 11:
                case 12:
                case 17:
                case 18:
                    o.writeShort(d.readUnsignedShort());
                    o.writeShort(d.readUnsignedShort());
                    break;
                case 5:
                case 6:
                    o.writeInt(d.readInt());
                    o.writeInt(d.readInt());
                    i++;
                    break;
                default:
                    throw new IOException("constant pool tag " + tag);
            }
        }
        o.writeShort(d.readUnsignedShort());
        o.writeShort(d.readUnsignedShort());
        o.writeShort(d.readUnsignedShort());
        int interfaces = d.readUnsignedShort();
        o.writeShort(interfaces);
        for (int i = 0; i < interfaces; i++) {
            o.writeShort(d.readUnsignedShort());
        }
        copyMembers(d, o, utf, null, null);
        copyMembers(d, o, utf, method, desc);
        copyAttributes(d, o, utf, -1);
        o.flush();
        if (d.read() != -1) {
            throw new IOException("trailing bytes");
        }
        return raw.toByteArray();
    }

    private static void copyMembers(
            DataInputStream d, DataOutputStream o, String[] utf, String method, String desc) throws IOException {
        int count = d.readUnsignedShort();
        o.writeShort(count);
        for (int i = 0; i < count; i++) {
            int access = d.readUnsignedShort();
            int name = d.readUnsignedShort();
            int descriptor = d.readUnsignedShort();
            o.writeShort(access);
            o.writeShort(name);
            o.writeShort(descriptor);
            boolean hit = method != null && method.equals(utf[name]) && desc.equals(utf[descriptor]);
            int locals = hit ? localsFor(access, utf[descriptor]) : -1;
            copyAttributes(d, o, utf, locals);
        }
    }

    private static void copyAttributes(DataInputStream d, DataOutputStream o, String[] utf, int locals)
            throws IOException {
        int count = d.readUnsignedShort();
        o.writeShort(count);
        for (int i = 0; i < count; i++) {
            int name = d.readUnsignedShort();
            int len = d.readInt();
            byte[] info = readFully(d, len);
            o.writeShort(name);
            if (locals >= 0 && "Code".equals(utf[name])) {
                byte[] code = {
                    0, 0,
                    (byte) (locals >> 8), (byte) locals,
                    0, 0, 0, 1, (byte) 0xB1,
                    0, 0,
                    0, 0
                };
                o.writeInt(code.length);
                o.write(code);
            } else {
                o.writeInt(info.length);
                o.write(info);
            }
        }
    }

    /** Argument slots, plus one for {@code this} unless the method is static. */
    private static int localsFor(int access, String desc) {
        int n = (access & 0x0008) == 0 ? 1 : 0;
        for (int i = 1; i < desc.length() && desc.charAt(i) != ')'; i++) {
            char c = desc.charAt(i);
            while (c == '[') {
                i++;
                c = desc.charAt(i);
            }
            if (c == 'J' || c == 'D') {
                n += 2;
            } else {
                n += 1;
            }
            if (c == 'L') {
                while (desc.charAt(i) != ';') {
                    i++;
                }
            }
        }
        return n;
    }

    private static byte[] readFully(DataInputStream d, int len) throws IOException {
        byte[] buf = new byte[len];
        d.readFully(buf);
        return buf;
    }
}
