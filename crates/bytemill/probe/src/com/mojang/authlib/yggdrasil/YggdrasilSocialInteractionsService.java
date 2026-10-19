package com.mojang.authlib.yggdrasil;

import com.mojang.authlib.Environment;
import com.mojang.authlib.minecraft.SocialInteractionsService;
import java.net.Proxy;
import java.util.UUID;

/**
 * Stands in front of authlib on the client classpath. The real class asks
 * api.minecraftservices.com whether this account may open a server connection.
 * An offline token gets "no", and 1.16.5 then ignores {@code --server}. This
 * copy allows the connection and never touches the network.
 */
public final class YggdrasilSocialInteractionsService implements SocialInteractionsService {
    public YggdrasilSocialInteractionsService(
            YggdrasilAuthenticationService authentication, String token, Environment environment) {}

    public YggdrasilSocialInteractionsService(String token, Proxy proxy, Environment environment) {}

    public boolean serversAllowed() {
        return true;
    }

    public boolean realmsAllowed() {
        return true;
    }

    public boolean chatAllowed() {
        return true;
    }

    public boolean telemetryAllowed() {
        return true;
    }

    public boolean isBlockedPlayer(UUID player) {
        return false;
    }
}
