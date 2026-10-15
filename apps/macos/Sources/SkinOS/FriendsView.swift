import SwiftUI

extension Presence {
    var color: Color {
        switch self { case .online: return Theme.accent; case .away: return .orange; case .busy: return .red; default: return Theme.muted }
    }
    var symbol: String {
        switch self { case .invisible: return "eye.slash"; case .online: return "circle.fill"; case .away: return "moon"; case .busy: return "minus.circle"; case .offline: return "circle" }
    }
}

struct PresenceMenu: View {
    @ObservedObject var model: AppModel
    var body: some View {
        Menu {
            ForEach(Presence.allCases) { presence in
                Button { model.changePresence(presence) } label: {
                    Label(presence.title, systemImage: presence == model.presence ? "checkmark" : presence.symbol)
                }
            }
        } label: {
            HStack(spacing: 7) {
                Circle().fill(model.presence.color).frame(width: 6, height: 6)
                Text(model.presencePending ? "Меняем статус…" : model.presence.title).font(.system(size: 11))
            }
        }.menuStyle(.borderlessButton).fixedSize().disabled(model.presencePending || model.account == nil || !model.coreReady)
    }
}

struct FriendsView: View {
    @ObservedObject var model: AppModel
    var body: some View {
        if model.account == nil { WelcomeView(model: model) }
        else {
            HStack(spacing: 0) {
                VStack(alignment: .leading, spacing: 0) {
                    HStack(alignment: .top) {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("Друзья").font(.system(size: 25, weight: .bold, design: .rounded))
                            Text("\(model.friends.filter { $0.state != .offline }.count) в сети · \(model.friends.count) всего")
                                .font(.system(size: 10)).foregroundStyle(Theme.muted)
                        }
                        Spacer()
                        Button { model.refreshFriends() } label: { Image(systemName: "arrow.clockwise") }
                            .buttonStyle(.plain).foregroundStyle(Theme.muted).disabled(model.friendsLoading).help("Обновить друзей")
                    }.padding(.bottom, 18)
                    PresenceMenu(model: model).padding(.bottom, 18)
                    HStack(spacing: 9) {
                        Image(systemName: "magnifyingglass").foregroundStyle(Theme.muted)
                        TextField("Найти друга", text: $model.friendSearch).textFieldStyle(.plain)
                    }.font(.system(size: 11)).padding(11).background(Theme.surface, in: RoundedRectangle(cornerRadius: 8)).padding(.bottom, 16)
                    if let error = model.friendsError {
                        Text(error).font(.system(size: 11)).foregroundStyle(.orange).padding(.bottom, 12)
                    }
                    if model.friendsLoading && !model.friendsLoaded {
                        ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                    } else if model.filteredFriends.isEmpty {
                        Text(model.friendsLoaded && model.friends.isEmpty ? "В списке пока нет друзей" : "Никого не найдено")
                            .font(.system(size: 12)).foregroundStyle(Theme.muted).frame(maxWidth: .infinity, maxHeight: .infinity)
                    } else {
                        ScrollView {
                            LazyVStack(spacing: 5) {
                                ForEach(model.filteredFriends) { friend in
                                    Button { model.selectFriend(friend) } label: { FriendRow(friend: friend, unread: model.unreadCounts[friend.id] ?? 0, selected: model.selectedFriendID == friend.id) }
                                        .buttonStyle(.plain)
                                        .contextMenu { Button("Открыть профиль") { model.showProfile(friend.id) } }
                                }
                            }
                        }.scrollIndicators(.hidden)
                    }
                }.padding(23).frame(width: 310)
                Rectangle().fill(Theme.divider).frame(width: 1)
                if let friend = model.selectedFriend {
                    ChatView(model: model, friend: friend).id(friend.id)
                } else {
                    EmptyState(symbol: "bubble.left.and.bubble.right", title: "Всегда на связи", subtitle: "Выберите друга, чтобы открыть переписку.\nПри входе ваш статус — «Невидимка».")
                }
            }
            .onAppear {
                model.loadCommunity()
                model.markVisibleChatRead()
            }
        }
    }
}

struct FriendRow: View {
    let friend: SteamFriend
    let unread: Int
    let selected: Bool
    var body: some View {
        HStack(spacing: 11) {
            FriendAvatar(friend: friend, size: 38)
            VStack(alignment: .leading, spacing: 5) {
                Text(friend.name).font(.system(size: 12, weight: .semibold)).foregroundStyle(.white).lineLimit(1)
                Text(friend.status).font(.system(size: 10)).foregroundStyle(friend.inGame ? Theme.accent : friend.state.color).lineLimit(1)
            }
            Spacer(minLength: 0)
            if unread > 0 { Text("\(unread)").font(.system(size: 10, weight: .bold)).padding(6).foregroundStyle(Theme.sidebar).background(Theme.accent, in: Circle()) }
        }.padding(10).background(selected ? Theme.elevated : .clear, in: RoundedRectangle(cornerRadius: 10)).contentShape(Rectangle())
    }
}

struct FriendAvatar: View {
    let friend: SteamFriend
    let size: CGFloat
    var body: some View {
        AvatarView(account: SteamAccount(id: friend.id, accountName: friend.name, displayName: friend.name, avatarURL: friend.avatarURL), size: size)
            .overlay(alignment: .bottomTrailing) { Circle().fill(friend.inGame ? Theme.accent : friend.state.color).frame(width: 9, height: 9).overlay(Circle().stroke(Theme.background, lineWidth: 2)).offset(x: 2, y: 2) }
    }
}

struct ChatView: View {
    @ObservedObject var model: AppModel
    let friend: SteamFriend
    @State private var draft = ""
    private var messages: [ChatMessage] { model.chats[friend.id] ?? [] }
    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 13) {
                FriendAvatar(friend: friend, size: 40)
                VStack(alignment: .leading, spacing: 5) {
                    Text(friend.name).font(.system(size: 17, weight: .semibold))
                    Text(friend.status).font(.system(size: 11)).foregroundStyle(friend.inGame ? Theme.accent : friend.state.color)
                }
                Spacer()
                Button { model.loadChat(friend.id) } label: { Image(systemName: "arrow.clockwise") }
                    .help("Обновить переписку").disabled(model.chatLoading.contains(friend.id))
                Button { model.showProfile(friend.id) } label: { Image(systemName: "person.crop.rectangle") }.help("Открыть профиль")
            }.buttonStyle(.plain).padding(24)
            Rectangle().fill(Theme.divider).frame(height: 1)
            if let error = model.chatErrors[friend.id] {
                HStack {
                    Text(error).font(.system(size: 11)).foregroundStyle(.orange)
                    Spacer()
                    Button { model.chatErrors[friend.id] = nil } label: { Image(systemName: "xmark") }.buttonStyle(.plain)
                }.padding(12).background(Color.orange.opacity(0.07))
            }
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 17) {
                        if model.chatLoading.contains(friend.id) { ProgressView().controlSize(.small).frame(maxWidth: .infinity).padding(12) }
                        if messages.isEmpty && !model.chatLoading.contains(friend.id) {
                            Text("Здесь появится ваша переписка").font(.system(size: 12)).foregroundStyle(Theme.muted).frame(maxWidth: .infinity).padding(.top, 40)
                        }
                        ForEach(messages) { message in
                            ChatBubble(message: message) { model.retryMessage(message, to: friend.id) }.id(message.id)
                        }
                        Color.clear.frame(height: 1).id("bottom")
                    }.padding(24)
                }.onChange(of: messages) { _, _ in withAnimation(.easeOut(duration: 0.15)) { proxy.scrollTo("bottom", anchor: .bottom) } }
                    .onAppear { proxy.scrollTo("bottom", anchor: .bottom) }
            }
            VStack(alignment: .trailing, spacing: 9) {
                HStack(alignment: .bottom, spacing: 12) {
                    TextField("Написать сообщение…", text: $draft, axis: .vertical)
                        .textFieldStyle(.plain).font(.system(size: 13)).lineLimit(1...6)
                        .onSubmit(send).padding(14).background(Theme.surface, in: RoundedRectangle(cornerRadius: 11))
                    Button(action: send) {
                        Image(systemName: "arrow.up").font(.system(size: 17, weight: .semibold)).frame(width: 42, height: 43)
                    }.buttonStyle(AccentButtonStyle()).disabled(!canSend).help("Отправить сообщение")
                }
                if draft.utf8.count > 6_000 { Text("Сообщение слишком длинное").font(.system(size: 10)).foregroundStyle(.orange) }
                else { Text("Enter — отправить · Shift+Enter — новая строка").font(.system(size: 9)).foregroundStyle(Theme.muted) }
            }.padding(22)
        }
    }
    private var canSend: Bool { !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && draft.utf8.count <= 6_000 && model.coreReady && model.account != nil }
    private func send() { guard canSend else { return }; model.sendChat(draft, to: friend.id); draft = "" }
}

struct ChatBubble: View {
    let message: ChatMessage
    let retry: () -> Void
    var body: some View {
        HStack {
            if message.outgoing { Spacer(minLength: 70) }
            VStack(alignment: message.outgoing ? .trailing : .leading, spacing: 6) {
                Text(message.text).font(.system(size: 13)).lineSpacing(3).textSelection(.enabled)
                    .padding(.horizontal, 15).padding(.vertical, 11)
                    .background(message.outgoing ? Theme.accent.opacity(0.12) : Theme.surface, in: RoundedRectangle(cornerRadius: 12))
                HStack(spacing: 7) {
                    Text(Date(timeIntervalSince1970: message.timestamp), format: .dateTime.day().month().hour().minute())
                    if message.outgoing {
                        if message.state == "sending" { Text("Отправка…") }
                        else if message.state == "failed" { Button("Не отправлено · Повторить", action: retry).buttonStyle(.plain).foregroundStyle(.orange) }
                        else { Image(systemName: "checkmark").foregroundStyle(Theme.accent).help("Принято Steam") }
                    }
                }.font(.system(size: 9)).foregroundStyle(Theme.muted)
            }
            if !message.outgoing { Spacer(minLength: 70) }
        }
    }
}
