using System.Runtime.InteropServices;
using System.Text;

namespace Chapbook;

/// <summary>One request the engine wants performed.</summary>
/// <param name="Url">
/// Opaque, and it may embed a per-user key: do not log it, and key any
/// credential by origin rather than by URL.
/// </param>
/// <param name="Headers">
/// What the engine wants sent, in order. Duplicates are meaningful.
/// </param>
public readonly record struct HttpRequest(
    string Url, IReadOnlyList<KeyValuePair<string, string>> Headers);

/// <summary>
/// The response being built, handed to a transport for the duration of one
/// callback.
/// </summary>
/// <remarks>
/// It is the one exception to "a transport must not call back into the
/// ABI": these builders, on this object, from inside the callback that was
/// given it. Using it after the callback returns is use-after-free.
/// </remarks>
public readonly ref struct HttpResponseBuilder
{
    private readonly nint _handle;

    internal HttpResponseBuilder(nint handle) => _handle = handle;

    /// <summary>
    /// The HTTP status, including 4xx and 5xx — those are responses, not
    /// failures, and the engine has its own flow for them.
    /// </summary>
    public void SetStatus(ushort status) =>
        ChapbookException.Check(
            Interop.cb_http_response_set_status(_handle, status), nameof(SetStatus));

    /// <summary>
    /// The <c>Content-Type</c> verbatim, parameters and all. Skip it when
    /// the server sent none; this value is authoritative over whatever the
    /// request asked for.
    /// </summary>
    public void SetContentType(string contentType) =>
        ChapbookException.Check(
            Interop.cb_http_response_set_content_type(_handle, contentType),
            nameof(SetContentType));

    /// <summary>Report one response header, name and value as received.</summary>
    /// <remarks>
    /// For the write half this is not optional. A Web Annotation container
    /// carries its whole concurrency story in <c>ETag</c> and says where it
    /// put a new mark in <c>Location</c>; a transport that discards them
    /// makes safe concurrent editing impossible, and the failure looks like
    /// sync quietly forgetting marks rather than like a missing header.
    /// </remarks>
    public void AddHeader(string name, string value) =>
        ChapbookException.Check(
            Interop.cb_http_response_add_header(_handle, name, value), nameof(AddHeader));

    /// <summary>Append body bytes. Call as many times as the body arrives in.</summary>
    public void AppendBody(ReadOnlySpan<byte> bytes)
    {
        if (bytes.IsEmpty)
        {
            return;
        }
        unsafe
        {
            fixed (byte* p = bytes)
            {
                ChapbookException.Check(
                    Interop.cb_http_response_append_body(_handle, (nint)p, (nuint)bytes.Length),
                    nameof(AppendBody));
            }
        }
    }

    /// <summary>
    /// The request never produced a response — DNS, the connection, a
    /// timeout. Not for 4xx or 5xx, which are responses.
    /// </summary>
    public void Fail(string message) =>
        ChapbookException.Check(
            Interop.cb_http_response_fail(_handle, message), nameof(Fail));
}

/// <summary>
/// The engine's networking, supplied by the host.
/// </summary>
/// <remarks>
/// <para>
/// A bundled networking stack is precisely the one a host cannot
/// substitute, so the engine opens no sockets of its own unless asked to.
/// On .NET the reason to substitute is concrete: an <see cref="HttpClient"/>
/// the app configured carries its proxy, its trust decisions, its
/// authentication handler and its timeouts, and a stack inside the engine
/// would carry none of them.
/// </para>
/// <para>
/// <b>The contract is unforgiving in three places.</b> A callback may fire
/// on any thread, including the engine's loader thread, and must
/// <i>block</i> until the transfer settles — this is not a place to return
/// a <see cref="Task"/>. It must not call back into this library except
/// through the <see cref="HttpResponseBuilder"/> it was handed. And it must
/// not retry: authentication retry is the engine's own flow one level up,
/// and a transport that retries turns one 401 into several.
/// </para>
/// </remarks>
public abstract class HttpTransport
{
    /// <summary>Perform a GET and report what came back.</summary>
    public abstract void Get(in HttpRequest request, HttpResponseBuilder response);

    /// <summary>
    /// Perform a POST, PUT or DELETE — the write half a sync transport must
    /// have, because reconciling marks means all three against a Web
    /// Annotation container and a position PUT against a progression
    /// service.
    /// </summary>
    /// <param name="method">
    /// An uppercase token, and only ever <c>POST</c>, <c>PUT</c> or
    /// <c>DELETE</c>.
    /// </param>
    /// <param name="body">The bytes to send; empty for a DELETE.</param>
    /// <remarks>
    /// Everything <see cref="Get"/> promises applies, plus reporting the
    /// response headers through
    /// <see cref="HttpResponseBuilder.AddHeader"/>.
    /// </remarks>
    public abstract void Send(
        string method, in HttpRequest request, ReadOnlySpan<byte> body,
        HttpResponseBuilder response);

    /// <summary>
    /// The engine has dropped this transport and will never call it again.
    /// </summary>
    /// <remarks>
    /// Called exactly once, from the ABI's finalizer: when the last session
    /// holding the transport closes, when a sync worker closes, or when a
    /// configuration is disposed without ever being opened. A host that
    /// created something for the engine's sake — a dedicated
    /// <see cref="HttpClient"/>, a handler with its own connection pool —
    /// releases it here, which is the only moment it is certain no callback
    /// is still running.
    /// </remarks>
    protected internal virtual void OnReleased()
    {
    }
}

/// <summary>
/// A transport over an <see cref="HttpClient"/> the host owns.
/// </summary>
/// <remarks>
/// The client is not disposed here: it belongs to whoever passed it, and
/// an <see cref="HttpClient"/> is meant to be long-lived and shared. A
/// service behind authentication wants one with a handler that attaches
/// it — no credential crosses this boundary, by design.
/// </remarks>
public class HttpClientTransport(HttpClient client) : HttpTransport
{
    private readonly HttpClient _client = client;

    /// <summary>A transport over a client this instance then owns for its lifetime.</summary>
    public HttpClientTransport() : this(new HttpClient())
    {
    }

    public override void Get(in HttpRequest request, HttpResponseBuilder response)
    {
        using var message = new HttpRequestMessage(HttpMethod.Get, request.Url);
        Apply(message, request.Headers);
        Perform(message, response);
    }

    public override void Send(
        string method, in HttpRequest request, ReadOnlySpan<byte> body,
        HttpResponseBuilder response)
    {
        using var message = new HttpRequestMessage(new HttpMethod(method), request.Url);
        Apply(message, request.Headers);
        if (!body.IsEmpty)
        {
            message.Content = new ByteArrayContent(body.ToArray());
        }
        Perform(message, response);
    }

    private static void Apply(
        HttpRequestMessage message, IReadOnlyList<KeyValuePair<string, string>> headers)
    {
        foreach ((string name, string value) in headers)
        {
            // Content headers are refused on the request collection, so
            // they go where they belong instead of being dropped.
            if (!message.Headers.TryAddWithoutValidation(name, value))
            {
                message.Content ??= new ByteArrayContent([]);
                message.Content.Headers.TryAddWithoutValidation(name, value);
            }
        }
    }

    private void Perform(HttpRequestMessage message, HttpResponseBuilder response)
    {
        try
        {
            // Blocking on purpose: the contract says this call must not
            // return until the transfer settles. It runs on an engine
            // thread with no synchronization context, so there is nothing
            // here to deadlock against.
            using HttpResponseMessage result =
                _client.Send(message, HttpCompletionOption.ResponseHeadersRead);

            response.SetStatus((ushort)result.StatusCode);
            if (result.Content.Headers.ContentType is { } contentType)
            {
                response.SetContentType(contentType.ToString());
            }
            foreach ((string name, IEnumerable<string> values) in
                     result.Headers.Concat(result.Content.Headers))
            {
                foreach (string value in values)
                {
                    response.AddHeader(name, value);
                }
            }

            using Stream stream = result.Content.ReadAsStream();
            byte[] buffer = new byte[64 * 1024];
            int read;
            while ((read = stream.Read(buffer, 0, buffer.Length)) > 0)
            {
                response.AppendBody(buffer.AsSpan(0, read));
            }
        }
        catch (Exception e) when (e is HttpRequestException or IOException
                                    or TaskCanceledException or InvalidOperationException)
        {
            // No response at all — DNS, the connection, a timeout. A 4xx
            // is not this; it went through `SetStatus` above.
            response.Fail(e.Message);
        }
    }
}

/// <summary>
/// The plumbing that puts a <see cref="HttpTransport"/> behind the ABI's
/// three function pointers.
/// </summary>
/// <remarks>
/// The transport instance is pinned by a <see cref="GCHandle"/> and that
/// handle is the ABI's <c>user</c> pointer. The engine calls
/// <c>finalize</c> exactly once when it drops the transport — a
/// configuration freed unopened, or the last session holding it closed —
/// and that is where the handle is released. This is the whole reason the
/// ABI has a finalizer: a host hands over a reference-counted object
/// without having to guess at the engine's lifetimes.
/// </remarks>
internal static unsafe class TransportBridge
{
    internal static nint Pin(HttpTransport transport) =>
        GCHandle.ToIntPtr(GCHandle.Alloc(transport));

    internal static delegate* unmanaged<nint, nint, nint, void> Get => &OnGet;

    internal static delegate* unmanaged<nint, nint, nint, nuint, nint, nint, void> Send => &OnSend;

    internal static delegate* unmanaged<nint, void> Finalize => &OnFinalize;

    [UnmanagedCallersOnly]
    private static void OnGet(nint request, nint response, nint user)
    {
        // Nothing may unwind into Rust. A host's exception becomes a
        // reported failure, which is a thing the engine can act on, rather
        // than a torn process.
        var builder = new HttpResponseBuilder(response);
        try
        {
            if (GCHandle.FromIntPtr(user).Target is HttpTransport transport)
            {
                transport.Get(Read(request), builder);
            }
        }
        catch (Exception e)
        {
            Report(builder, e);
        }
    }

    [UnmanagedCallersOnly]
    private static void OnSend(
        nint method, nint request, nint body, nuint bodyLength, nint response, nint user)
    {
        var builder = new HttpResponseBuilder(response);
        try
        {
            if (GCHandle.FromIntPtr(user).Target is HttpTransport transport)
            {
                ReadOnlySpan<byte> bytes = body == 0
                    ? []
                    : new ReadOnlySpan<byte>((void*)body, checked((int)bodyLength));
                transport.Send(
                    Marshal.PtrToStringUTF8(method) ?? "GET", Read(request), bytes, builder);
            }
        }
        catch (Exception e)
        {
            Report(builder, e);
        }
    }

    [UnmanagedCallersOnly]
    private static void OnFinalize(nint user)
    {
        try
        {
            if (user != 0)
            {
                GCHandle handle = GCHandle.FromIntPtr(user);
                (handle.Target as HttpTransport)?.OnReleased();
                handle.Free();
            }
        }
        catch
        {
            // Deliberately swallowed; see above.
        }
    }

    private static void Report(HttpResponseBuilder builder, Exception e)
    {
        try
        {
            builder.Fail(e.Message);
        }
        catch
        {
            // The response may already have been failed or completed. There
            // is nowhere left to say anything.
        }
    }

    private static HttpRequest Read(nint request)
    {
        NativeHttpRequest native = *(NativeHttpRequest*)request;
        string url = Marshal.PtrToStringUTF8(native.Url) ?? string.Empty;
        var headers = new List<KeyValuePair<string, string>>((int)native.HeaderCount);
        for (nuint i = 0; i < native.HeaderCount; i++)
        {
            NativeHttpHeader header = ((NativeHttpHeader*)native.Headers)[i];
            headers.Add(new KeyValuePair<string, string>(
                Marshal.PtrToStringUTF8(header.Name) ?? string.Empty,
                Marshal.PtrToStringUTF8(header.Value) ?? string.Empty));
        }
        return new HttpRequest(url, headers);
    }
}

public sealed partial class SessionConfiguration
{
    /// <summary>
    /// Fetch through the host's networking instead of the bundled
    /// transport — for OPDS catalogues and streamed comic pages.
    /// </summary>
    /// <remarks>
    /// The transport is owned by the engine from this call onward and is
    /// released when the last session holding it closes, or when this
    /// configuration is disposed unopened.
    /// </remarks>
    public SessionConfiguration WithTransport(HttpTransport transport)
    {
        ArgumentNullException.ThrowIfNull(transport);
        nint user = TransportBridge.Pin(transport);
        unsafe
        {
            ChapbookException.Check(
                Interop.cb_config_set_http_transport(
                    Live(),
                    (nint)TransportBridge.Get,
                    (nint)TransportBridge.Finalize,
                    user),
                nameof(WithTransport));
        }
        return this;
    }
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeHttpHeader
{
    public nint Name;
    public nint Value;
}

[StructLayout(LayoutKind.Sequential)]
internal struct NativeHttpRequest
{
    public nint Url;
    public nint Headers;
    public nuint HeaderCount;
}
