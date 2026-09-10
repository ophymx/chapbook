using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.RegularExpressions;
using Chapbook;
using Xunit;

namespace Chapbook.Tests;

/// <summary>
/// The checked-in header, read back and compared against the structs this
/// binding declares.
/// </summary>
/// <remarks>
/// <para>
/// This is the .NET half of what <c>chapbook-ffi/tests/header.rs</c> does
/// for the Rust half: the header is the contract, and something has to
/// notice when a transcription of it drifts. Disabling runtime marshalling
/// makes a <i>non-blittable</i> field a build error, which is most of the
/// battle — but it cannot see a field <b>inserted into the middle</b> of a
/// struct, and that is the change this exists for.
/// </para>
/// <para>
/// It is not hypothetical. `cb_sync_report` grew `marks_withdrawn` after
/// `marks_refreshed` and `listing_truncated` after `marks_conflicts`, and
/// a binding still holding the old shape read every field after the first
/// one from the wrong offset — reporting plausible numbers, not a crash.
/// Two behavioural tests happened to catch it; this one names the struct
/// and the field instead of failing somewhere downstream. `cb_abi_version`
/// could not have helped: it tracks the workspace version, which has not
/// moved since the ABI was written.
/// </para>
/// </remarks>
public class LayoutTests
{
    /// <summary>
    /// Which C# struct transcribes which C one. A struct absent here is
    /// one this binding does not pass by value.
    /// </summary>
    private static readonly (string C, Type Managed)[] Transcribed =
    [
        ("cb_metrics", typeof(NativeMetrics)),
        ("cb_position", typeof(NativePosition)),
        ("cb_settings", typeof(NativeSettings)),
        ("cb_rect", typeof(NativeRect)),
        ("cb_text_run", typeof(NativeTextRun)),
        ("cb_word_span", typeof(NativeWordSpan)),
        ("cb_session_event", typeof(NativeSessionEvent)),
        ("cb_book_query", typeof(NativeBookQuery)),
        ("cb_book", typeof(NativeBook)),
        ("cb_collection", typeof(NativeCollection)),
        ("cb_sync_report", typeof(NativeSyncReport)),
        ("cb_http_header", typeof(NativeHttpHeader)),
        ("cb_http_request", typeof(NativeHttpRequest)),
        ("cb_annotation", typeof(NativeAnnotation)),
        ("cb_toc_entry", typeof(NativeTocEntry)),
        ("cb_search_hit", typeof(NativeSearchHit)),
        ("cb_entry", typeof(NativeEntry)),
        ("cb_facet", typeof(NativeFacet)),
    ];

    [Theory]
    [MemberData(nameof(Structs))]
    public void EveryStructMatchesTheHeaderFieldForField(string name, Type managed)
    {
        IReadOnlyList<(string Name, string Type)> declared = HeaderFields(name);
        FieldInfo[] transcribed = managed
            .GetFields(BindingFlags.Public | BindingFlags.Instance);

        Assert.Equal(declared.Count, transcribed.Length);
        for (int i = 0; i < declared.Count; i++)
        {
            (string field, string cType) = declared[i];

            // Order is the whole point: a field in the wrong place reads
            // the wrong bytes and reports nothing.
            Assert.Equal(Pascal(field), transcribed[i].Name);

            int expected = SizeOfC(cType);
            int actual = SizeOfManaged(transcribed[i].FieldType);
            Assert.True(
                expected == actual,
                $"{name}.{field} is `{cType}` ({expected} bytes) but " +
                $"{managed.Name}.{transcribed[i].Name} is " +
                $"{transcribed[i].FieldType.Name} ({actual} bytes)");
        }
    }

    public static TheoryData<string, Type> Structs()
    {
        var data = new TheoryData<string, Type>();
        foreach ((string c, Type managed) in Transcribed)
        {
            data.Add(c, managed);
        }
        return data;
    }

    /// <summary>The fields of one <c>typedef struct</c>, in order.</summary>
    private static IReadOnlyList<(string Name, string Type)> HeaderFields(string name)
    {
        string header = File.ReadAllText(Fixture.Header());
        Match body = Regex.Match(
            header,
            @"typedef struct " + Regex.Escape(name) + @" \{(.*?)\n\} " + Regex.Escape(name) + ";",
            RegexOptions.Singleline);
        Assert.True(body.Success, $"{name} is not in the header any more");

        // Strip the doc comments, then take one declaration per line.
        string fields = Regex.Replace(body.Groups[1].Value, @"/\*.*?\*/", "", RegexOptions.Singleline);
        var declared = new List<(string, string)>();
        foreach (string line in fields.Split('\n'))
        {
            string text = line.Trim().TrimEnd(';');
            if (text.Length == 0)
            {
                continue;
            }
            int split = text.LastIndexOf(' ');
            Assert.True(split > 0, $"cannot read `{text}` in {name}");
            string type = text[..split].Trim();
            string field = text[(split + 1)..].Trim();
            if (field.StartsWith('*'))
            {
                // `const char *detail` — the star belongs to the type.
                type += " *";
                field = field[1..];
            }
            declared.Add((field, type));
        }
        Assert.NotEmpty(declared);
        return declared;
    }

    /// <summary>
    /// What one C type occupies on the targets this binding runs on, which
    /// are 64-bit.
    /// </summary>
    private static int SizeOfC(string type) => type switch
    {
        "int8_t" or "uint8_t" or "bool" or "char" => 1,
        "int16_t" or "uint16_t" => 2,
        "int32_t" or "uint32_t" or "float" => 4,
        "int64_t" or "uint64_t" or "double" or "size_t" or "uintptr_t" => 8,
        // Every pointer, however it was spelled.
        _ when type.EndsWith('*') => 8,
        "struct cb_rect" => SizeOfManaged(typeof(NativeRect)),
        // Every other `cb_*` in a struct is one of this ABI's enums,
        // whether the header spelled it `enum cb_theme` or bare as the
        // typedef. A C `enum` is an `int` and cbindgen's `repr(u32)` ones
        // are four bytes too, so both land here.
        _ when type.StartsWith("enum cb_") || type.StartsWith("cb_") => 4,
        _ => throw new Xunit.Sdk.XunitException($"no size known for C type `{type}`"),
    };

    /// <summary>
    /// What one managed field occupies. <see cref="Marshal.SizeOf(Type)"/>
    /// refuses an enum outright and answers the pointer size for
    /// <c>nint</c> only by accident of the platform, so both are handled
    /// before it is asked.
    /// </summary>
    private static int SizeOfManaged(Type type)
    {
        if (type.IsEnum)
        {
            return SizeOfManaged(Enum.GetUnderlyingType(type));
        }
        if (type == typeof(nint) || type == typeof(nuint))
        {
            return IntPtr.Size;
        }
        return Marshal.SizeOf(type);
    }

    /// <summary><c>marks_withdrawn</c> → <c>MarksWithdrawn</c>.</summary>
    private static string Pascal(string snake) =>
        string.Concat(snake.Split('_').Select(
            part => part.Length == 0 ? part : char.ToUpperInvariant(part[0]) + part[1..]));
}
