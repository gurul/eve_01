// compactLinks and linkLabel, case for case with the web chat's links.test.ts, plus the open code fence.
import Testing
@testable import EveMarkdown

@Suite struct LinksTests {
    @Test func bareMapsURLBecomesAShortMapsLabel() {
        let md = "Your class is in GUG 220. https://www.google.com/maps/search/?api=1&query=GUG%20220"
        #expect(compactLinks(md)
            == "Your class is in GUG 220. [Maps · GUG 220](https://www.google.com/maps/search/?api=1&query=GUG%20220)")
    }

    @Test func otherLinksAreHostAndFirstSegmentWithPunctuationOutside() {
        #expect(compactLinks("see https://docs.example.com/guide/setup.")
            == "see [docs.example.com › guide](https://docs.example.com/guide/setup).")
    }

    @Test func markdownLinksAngleLinksAndCodeAreLeftAlone() {
        let md = "[here](https://a.example.com/x) <https://b.example.com> `https://c.example.com` ```\nhttps://d.example.com\n```"
        #expect(compactLinks(md) == md)
    }

    @Test func positiveControlTheSameURLOutsideCodeIsRewritten() {
        #expect(compactLinks("https://c.example.com") == "[c.example.com](https://c.example.com)")
    }

    @Test func survivesAMalformedURL() {
        #expect(linkLabel("https://") == "https://")
    }

    @Test func survivesASegmentThatIsNotValidPercentEncoding() {
        #expect(linkLabel("https://shop.com/50%off") == "shop.com › 50%off")
        #expect(compactLinks("see https://shop.com/50%off") == "see [shop.com › 50%off](https://shop.com/50%off)")
    }

    @Test func positiveControlAValidEscapeIsStillDecoded() {
        #expect(linkLabel("https://shop.com/caf%C3%A9") == "shop.com › café")
    }

    // Beyond the web tests.

    @Test func labelsMapsYouTubeAndLongSegments() {
        #expect(linkLabel("https://maps.apple.com/?q=Main+Library") == "Maps · Main Library")
        #expect(linkLabel("https://maps.google.com/") == "Maps")
        #expect(linkLabel("https://www.youtube.com/watch?v=abc") == "YouTube")
        #expect(linkLabel("https://youtu.be/abc") == "YouTube")
        #expect(linkLabel("https://Example.com:8080/a-path-segment-that-is-far-too-long/x") == "example.com")
        #expect(linkLabel("https://example.com") == "example.com")
        #expect(linkLabel("not a url") == "not a url")
    }

    @Test func bracketsInALabelAreDropped() {
        // As on the web, the address stops at a "]"; the "[" it keeps is not in the label.
        #expect(compactLinks("(https://example.com/[x])") == "([example.com › x](https://example.com/[x)])")
    }

    @Test func aFenceThatIsStillOpenIsCode() {
        let md = "```sh\ncurl https://example.com/install"
        #expect(compactLinks(md) == md)
    }

    @Test func aLongDocumentWithoutLinksComesBackAsIs() {
        let md = String(repeating: "plain words and `code` here. ", count: 200)
        #expect(compactLinks(md) == md)
    }
}
