package probe;

import java.lang.instrument.ClassFileTransformer;
import java.lang.instrument.Instrumentation;
import java.security.ProtectionDomain;

/**
 * 1.17 calls {@code clearLevel} from {@code ConnectScreen.startConnecting}
 * while {@code Minecraft} is still being constructed. That draw runs before the
 * renderer exists and the process crashes. There is no level to clear yet, so
 * the call is a no-op and the connection thread still starts.
 */
public final class ClientPatch implements ClassFileTransformer {
    public static void premain(String args, Instrumentation inst) {
        if ("1.17".equals(args)) {
            inst.addTransformer(new ClientPatch());
        }
    }

    @Override
    public byte[] transform(
            ClassLoader loader,
            String className,
            Class<?> classBeingRedefined,
            ProtectionDomain protectionDomain,
            byte[] classfileBuffer) {
        if ("dvo".equals(className)) {
            return ClassPatch.nopMethod(classfileBuffer, "r", "()V");
        }
        return null;
    }
}
