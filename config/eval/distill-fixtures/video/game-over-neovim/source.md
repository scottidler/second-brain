[00:00:01] We're<00:00:00.400> finally<00:00:00.640> getting<00:00:00.960> plugins<00:00:01.439> for<00:00:01.680> the We're finally getting plugins for the
[00:00:01] We're finally getting plugins for the beloved<00:00:02.480> Helix<00:00:02.960> text<00:00:03.280> editor.<00:00:04.160> This<00:00:04.400> PR<00:00:04.720> has
[00:00:04] beloved Helix text editor. This PR has
[00:00:04] beloved Helix text editor. This PR has been<00:00:05.040> open<00:00:05.279> for<00:00:05.600> almost<00:00:05.920> 3<00:00:06.080> years<00:00:06.240> now.<00:00:06.480> It<00:00:06.640> was
[00:00:06] been open for almost 3 years now. It was
[00:00:06] been open for almost 3 years now. It was open<00:00:06.960> on<00:00:07.120> October<00:00:07.600> 30th,<00:00:08.000> 2023.<00:00:08.960> This<00:00:09.120> is<00:00:09.280> not
[00:00:09] open on October 30th, 2023. This is not
[00:00:09] open on October 30th, 2023. This is not a<00:00:09.599> dramafree<00:00:10.320> poll<00:00:10.559> request.<00:00:10.960> I<00:00:11.040> got<00:00:11.120> to<00:00:11.280> warn
[00:00:11] a dramafree poll request. I got to warn
[00:00:11] a dramafree poll request. I got to warn you,<00:00:11.599> we're<00:00:11.759> going<00:00:11.840> to<00:00:11.920> talk<00:00:12.000> a<00:00:12.160> little<00:00:12.240> bit
[00:00:12] you, we're going to talk a little bit
[00:00:12] you, we're going to talk a little bit about<00:00:12.559> that<00:00:12.800> later.<00:00:13.280> Now,<00:00:13.440> I<00:00:13.599> spoke<00:00:13.759> with<00:00:13.920> Matt
[00:00:14] about that later. Now, I spoke with Matt
[00:00:14] about that later. Now, I spoke with Matt Paris,<00:00:14.639> the<00:00:14.799> author<00:00:15.040> of<00:00:15.200> this<00:00:15.440> PR<00:00:15.839> and<00:00:16.160> who
[00:00:16] Paris, the author of this PR and who
[00:00:16] Paris, the author of this PR and who also<00:00:16.720> happens<00:00:17.039> to<00:00:17.199> be<00:00:17.279> the<00:00:17.520> author<00:00:17.840> of<00:00:18.000> the
[00:00:18] also happens to be the author of the
[00:00:18] also happens to be the author of the language<00:00:18.560> that<00:00:18.800> was<00:00:18.960> chosen<00:00:19.199> for<00:00:19.359> this
[00:00:19] language that was chosen for this
[00:00:19] language that was chosen for this plug-in<00:00:19.920> system.<00:00:20.560> We're<00:00:20.720> going<00:00:20.800> to<00:00:20.960> talk
[00:00:21] plug-in system. We're going to talk
[00:00:21] plug-in system. We're going to talk about<00:00:21.279> the<00:00:21.520> language,<00:00:22.400> how<00:00:22.560> to<00:00:22.640> write
[00:00:22] about the language, how to write
[00:00:22] about the language, how to write plugins,<00:00:23.359> how<00:00:23.519> to<00:00:23.680> manage<00:00:24.000> community<00:00:24.400> written
[00:00:24] plugins, how to manage community written
[00:00:24] plugins, how to manage community written plugins.<00:00:25.279> One<00:00:25.600> caveat<00:00:26.000> is<00:00:26.160> that<00:00:26.320> this<00:00:26.480> PR<00:00:26.800> is
[00:00:26] plugins. One caveat is that this PR is
[00:00:26] plugins. One caveat is that this PR is not<00:00:27.119> even<00:00:27.279> merged<00:00:27.680> yet.<00:00:28.560> Most<00:00:28.800> of<00:00:28.880> the
[00:00:29] not even merged yet. Most of the
[00:00:29] not even merged yet. Most of the broadstrokes<00:00:29.679> are<00:00:30.400> pretty<00:00:30.640> much<00:00:30.800> solidified,
[00:00:31] broadstrokes are pretty much solidified,
[00:00:31] broadstrokes are pretty much solidified, the<00:00:31.599> language<00:00:32.000> chosen<00:00:32.559> and<00:00:33.120> the<00:00:33.280> the<00:00:33.600> highle
[00:00:34] the language chosen and the the highle
[00:00:34] the language chosen and the the highle way<00:00:34.320> things<00:00:34.559> are<00:00:34.719> going<00:00:34.800> to<00:00:34.880> work.<00:00:35.440> Some<00:00:35.600> of
[00:00:35] way things are going to work. Some of
[00:00:35] way things are going to work. Some of the<00:00:35.840> [[low-level]]<00:00:36.320> details<00:00:36.800> might<00:00:37.040> change<00:00:37.280> a
[00:00:37] the low-level details might change a
[00:00:37] the low-level details might change a little<00:00:37.520> bit<00:00:37.760> between<00:00:38.320> now<00:00:38.640> and<00:00:38.960> when<00:00:39.200> this<00:00:39.440> is
[00:00:39] little bit between now and when this is
[00:00:39] little bit between now and when this is actually<00:00:39.840> merged,<00:00:40.320> but<00:00:40.879> most<00:00:41.120> of<00:00:41.280> what<00:00:41.440> I'm
[00:00:41] actually merged, but most of what I'm
[00:00:41] actually merged, but most of what I'm going<00:00:41.680> to<00:00:41.760> talk<00:00:41.920> about<00:00:42.320> should<00:00:42.559> still<00:00:42.800> remain
[00:00:43] going to talk about should still remain
[00:00:43] going to talk about should still remain true<00:00:43.280> after<00:00:43.600> this<00:00:43.760> is<00:00:43.920> merged.<00:00:44.399> Now,<00:00:44.559> if<00:00:44.719> you
[00:00:44] true after this is merged. Now, if you
[00:00:44] true after this is merged. Now, if you have<00:00:44.960> no<00:00:45.120> idea<00:00:45.360> what<00:00:45.520> I'm<00:00:45.680> talking<00:00:45.840> about,
[00:00:46] have no idea what I'm talking about,
[00:00:46] have no idea what I'm talking about, Helix<00:00:46.559> is<00:00:46.800> a<00:00:46.960> postmodern<00:00:47.600> text<00:00:47.840> editor.<00:00:48.239> In
[00:00:48] Helix is a postmodern text editor. In
[00:00:48] Helix is a postmodern text editor. In their<00:00:48.559> words,<00:00:48.879> in<00:00:49.120> my<00:00:49.280> words,<00:00:49.520> it's<00:00:49.760> basically
[00:00:49] their words, in my words, it's basically
[00:00:50] their words, in my words, it's basically an<00:00:50.239> IDE.<00:00:51.039> You<00:00:51.200> could<00:00:51.360> say<00:00:51.440> it's<00:00:51.680> a<00:00:51.840> competitor
[00:00:52] an IDE. You could say it's a competitor
[00:00:52] an IDE. You could say it's a competitor to<00:00:52.559> editors<00:00:52.960> like<00:00:53.199> Vim<00:00:53.600> and<00:00:53.840> Emacs.<00:00:54.640> Helix<00:00:55.199> is
[00:00:55] to editors like Vim and Emacs. Helix is
[00:00:55] to editors like Vim and Emacs. Helix is a<00:00:55.600> modal<00:00:55.920> editor,<00:00:56.320> kind<00:00:56.399> of<00:00:56.480> like<00:00:56.640> Vim.<00:00:57.039> I
[00:00:57] a modal editor, kind of like Vim. I
[00:00:57] a modal editor, kind of like Vim. I start<00:00:57.440> out<00:00:57.520> in<00:00:57.760> normal<00:00:58.000> mode<00:00:58.160> and<00:00:58.320> I<00:00:58.480> can<00:00:58.559> enter
[00:00:58] start out in normal mode and I can enter
[00:00:58] start out in normal mode and I can enter insert<00:00:59.120> mode<00:00:59.280> by<00:00:59.520> pressing<00:00:59.840> I<00:01:00.239> and<00:01:00.480> then<00:01:00.640> I<00:01:00.800> can
[00:01:00] insert mode by pressing I and then I can
[00:01:00] insert mode by pressing I and then I can type<00:01:01.120> some<00:01:01.280> characters.<00:01:02.320> Escape<00:01:02.719> to<00:01:02.960> get<00:01:03.120> out
[00:01:03] type some characters. Escape to get out
[00:01:03] type some characters. Escape to get out of<00:01:03.359> insert<00:01:03.760> mode<00:01:04.000> back<00:01:04.159> to<00:01:04.320> normal<00:01:04.640> mode.<00:01:05.199> U<00:01:05.519> to
[00:01:05] of insert mode back to normal mode. U to
[00:01:05] of insert mode back to normal mode. U to undo<00:01:06.159> things<00:01:06.400> like<00:01:06.560> this.<00:01:06.960> I<00:01:07.200> can<00:01:07.360> move<00:01:07.520> around
[00:01:07] undo things like this. I can move around
[00:01:07] undo things like this. I can move around using<00:01:08.159> hjk<00:01:08.720> and<00:01:08.960> l<00:01:09.200> to<00:01:09.360> move<00:01:09.520> around<00:01:09.680> by<00:01:09.920> one
[00:01:10] using hjk and l to move around by one
[00:01:10] using hjk and l to move around by one character.<00:01:10.640> I<00:01:10.880> can<00:01:11.040> press<00:01:11.200> B<00:01:11.439> to<00:01:11.600> move<00:01:11.760> back<00:01:11.840> a
[00:01:11] character. I can press B to move back a
[00:01:12] character. I can press B to move back a word.<00:01:12.320> W<00:01:12.560> to<00:01:12.720> move<00:01:12.880> forward<00:01:13.119> a<00:01:13.280> word.<00:01:13.760> But
[00:01:13] word. W to move forward a word. But
[00:01:13] word. W to move forward a word. But you'll<00:01:14.159> notice<00:01:14.479> as<00:01:14.720> I<00:01:14.880> traverse<00:01:15.280> a<00:01:15.520> word,<00:01:16.159> the
[00:01:16] you'll notice as I traverse a word, the
[00:01:16] you'll notice as I traverse a word, the word<00:01:16.560> that<00:01:16.720> I've<00:01:16.880> just<00:01:17.040> traversed<00:01:17.520> gets
[00:01:17] word that I've just traversed gets
[00:01:17] word that I've just traversed gets highlighted.<00:01:18.479> Now<00:01:18.720> in<00:01:18.880> Neovim,<00:01:19.520> if<00:01:19.600> you
[00:01:19] highlighted. Now in Neovim, if you
[00:01:19] highlighted. Now in Neovim, if you wanted<00:01:19.920> to<00:01:20.080> delete<00:01:20.320> a<00:01:20.479> word,<00:01:20.720> you'd<00:01:21.040> do<00:01:21.200> DW.<00:01:22.000> In
[00:01:22] wanted to delete a word, you'd do DW. In
[00:01:22] wanted to delete a word, you'd do DW. In Helix,<00:01:22.960> I<00:01:23.200> do<00:01:23.439> W,<00:01:24.159> which<00:01:24.400> highlights<00:01:24.799> the
[00:01:25] Helix, I do W, which highlights the
[00:01:25] Helix, I do W, which highlights the word,<00:01:25.360> and<00:01:25.600> then<00:01:25.759> I<00:01:25.920> do<00:01:26.159> D.<00:01:27.040> So,<00:01:27.920> Neovim<00:01:28.640> is
[00:01:29] word, and then I do D. So, Neovim is
[00:01:29] word, and then I do D. So, Neovim is action<00:01:30.560> object.<00:01:31.439> Helix<00:01:32.000> is<00:01:32.799> object<00:01:33.360> action.
[00:01:33] action object. Helix is object action.
[00:01:33] action object. Helix is object action. So,<00:01:33.920> I<00:01:34.079> actually<00:01:34.320> get<00:01:34.479> to<00:01:34.640> see<00:01:35.200> what<00:01:35.520> I'm<00:01:35.680> about
[00:01:35] So, I actually get to see what I'm about
[00:01:35] So, I actually get to see what I'm about to<00:01:36.159> perform<00:01:36.479> an<00:01:36.640> action<00:01:36.880> on<00:01:37.200> before<00:01:37.520> I<00:01:37.759> perform
[00:01:38] to perform an action on before I perform
[00:01:38] to perform an action on before I perform that<00:01:38.320> action.<00:01:39.119> Some<00:01:39.439> people<00:01:39.600> prefer<00:01:39.920> that.<00:01:40.159> I
[00:01:40] that action. Some people prefer that. I
[00:01:40] that action. Some people prefer that. I can<00:01:40.479> see<00:01:40.560> the<00:01:40.720> appeal<00:01:41.040> to<00:01:41.200> it.<00:01:41.840> There<00:01:42.000> are<00:01:42.240> some
[00:01:42] can see the appeal to it. There are some
[00:01:42] can see the appeal to it. There are some downsides,<00:01:43.040> but<00:01:43.280> that's<00:01:43.439> a<00:01:43.600> story<00:01:43.759> for
[00:01:43] downsides, but that's a story for
[00:01:43] downsides, but that's a story for another<00:01:44.240> video.<00:01:44.720> One<00:01:44.960> aspect<00:01:45.280> of<00:01:45.439> the<00:01:45.600> Helix
[00:01:45] another video. One aspect of the Helix
[00:01:46] another video. One aspect of the Helix ethos<00:01:46.399> so<00:01:46.640> far<00:01:46.799> is<00:01:47.119> to<00:01:47.439> build<00:01:47.759> everything<00:01:48.159> into
[00:01:48] ethos so far is to build everything into
[00:01:48] ethos so far is to build everything into the<00:01:48.640> editor<00:01:49.280> instead<00:01:49.680> of<00:01:49.920> implementing<00:01:50.399> it<00:01:50.560> as
[00:01:50] the editor instead of implementing it as
[00:01:50] the editor instead of implementing it as a<00:01:50.960> plug-in.<00:01:51.840> There<00:01:52.079> are<00:01:52.240> some<00:01:52.399> advantages<00:01:52.799> to
[00:01:53] a plug-in. There are some advantages to
[00:01:53] a plug-in. There are some advantages to that,<00:01:53.439> it's<00:01:53.680> very<00:01:53.920> performant<00:01:54.479> because
[00:01:54] that, it's very performant because
[00:01:54] that, it's very performant because everything<00:01:55.119> is<00:01:55.360> implemented<00:01:55.759> in<00:01:56.000> native
[00:01:56] everything is implemented in native
[00:01:56] everything is implemented in native Rust.<00:01:57.439> It's<00:01:57.680> native<00:01:58.000> machine<00:01:58.320> code<00:01:58.640> and
[00:01:58] Rust. It's native machine code and
[00:01:58] Rust. It's native machine code and things<00:01:59.200> like<00:01:59.360> a<00:01:59.600> file<00:01:59.920> picker.<00:02:00.560> This<00:02:00.719> is<00:02:00.799> not<00:02:00.880> a
[00:02:01] things like a file picker. This is not a
[00:02:01] things like a file picker. This is not a plugin.<00:02:01.439> This<00:02:01.520> is<00:02:01.680> built<00:02:01.920> directly<00:02:02.159> into<00:02:02.399> the
[00:02:02] plugin. This is built directly into the
[00:02:02] plugin. This is built directly into the editor.<00:02:03.360> Um<00:02:03.920> LSP<00:02:04.479> support,<00:02:05.280> git<00:02:05.600> integration,
[00:02:06] editor. Um LSP support, git integration,
[00:02:06] editor. Um LSP support, git integration, everything<00:02:06.719> is<00:02:06.960> lightning<00:02:07.439> fast.<00:02:07.759> And<00:02:07.920> if<00:02:08.000> you
[00:02:08] everything is lightning fast. And if you
[00:02:08] everything is lightning fast. And if you use<00:02:08.239> Neovm,<00:02:08.879> you<00:02:09.039> might<00:02:09.200> think<00:02:09.360> that's<00:02:09.679> kind
[00:02:09] use Neovm, you might think that's kind
[00:02:09] use Neovm, you might think that's kind of<00:02:09.920> the<00:02:10.560> upper<00:02:10.879> bound<00:02:11.200> in<00:02:11.440> terms<00:02:11.680> of
[00:02:11] of the upper bound in terms of
[00:02:11] of the upper bound in terms of performance<00:02:12.239> of<00:02:12.400> a<00:02:12.560> text<00:02:12.800> editor,<00:02:13.520> but<00:02:13.680> then
[00:02:13] performance of a text editor, but then
[00:02:13] performance of a text editor, but then you<00:02:13.920> open<00:02:14.160> Helix<00:02:14.560> and<00:02:14.800> it's<00:02:15.040> actually
[00:02:15] you open Helix and it's actually
[00:02:15] you open Helix and it's actually noticeably<00:02:15.920> faster.<00:02:16.400> Not<00:02:16.560> like<00:02:16.879> light<00:02:17.040> years
[00:02:17] noticeably faster. Not like light years
[00:02:17] noticeably faster. Not like light years faster,<00:02:17.680> but<00:02:17.840> it's<00:02:17.920> it's<00:02:18.319> noticeable<00:02:18.720> the
[00:02:18] faster, but it's it's noticeable the
[00:02:18] faster, but it's it's noticeable the performance<00:02:19.360> difference.<00:02:20.000> Helix<00:02:20.560> is,<00:02:20.959> like<00:02:21.120> I
[00:02:21] performance difference. Helix is, like I
[00:02:21] performance difference. Helix is, like I said<00:02:21.440> in<00:02:21.599> a<00:02:21.760> previous<00:02:22.000> video,<00:02:22.400> the<00:02:22.720> fastest
[00:02:23] said in a previous video, the fastest
[00:02:23] said in a previous video, the fastest editor<00:02:23.680> I<00:02:23.840> have<00:02:24.000> ever<00:02:24.239> used.<00:02:24.720> Now,<00:02:24.959> let's<00:02:25.120> talk
[00:02:25] editor I have ever used. Now, let's talk
[00:02:25] editor I have ever used. Now, let's talk about<00:02:25.440> the<00:02:25.680> language<00:02:26.000> that<00:02:26.239> was<00:02:26.400> chosen<00:02:26.640> for
[00:02:26] about the language that was chosen for
[00:02:26] about the language that was chosen for Helix<00:02:27.280> plugins.<00:02:27.760> And<00:02:27.840> that<00:02:28.000> language<00:02:28.319> is
[00:02:28] Helix plugins. And that language is
[00:02:28] Helix plugins. And that language is Steel.<00:02:28.959> And<00:02:29.120> the<00:02:29.280> Steel<00:02:29.520> language<00:02:29.840> was
[00:02:30] Steel. And the Steel language was
[00:02:30] Steel. And the Steel language was actually<00:02:30.319> built<00:02:30.560> by<00:02:30.800> Matt<00:02:31.120> Paris<00:02:31.520> long<00:02:31.760> before
[00:02:32] actually built by Matt Paris long before
[00:02:32] actually built by Matt Paris long before Helix<00:02:32.879> plugins<00:02:33.280> came<00:02:33.440> along.<00:02:33.920> And<00:02:34.160> steel<00:02:34.480> in
[00:02:34] Helix plugins came along. And steel in
[00:02:34] Helix plugins came along. And steel in this<00:02:34.800> context<00:02:35.200> is<00:02:35.440> both<00:02:35.599> a<00:02:35.840> language<00:02:36.239> and<00:02:36.480> a
[00:02:36] this context is both a language and a
[00:02:36] this context is both a language and a runtime.<00:02:37.360> It's<00:02:37.680> a<00:02:37.920> runtime<00:02:38.319> that<00:02:38.560> you<00:02:38.720> can
[00:02:38] runtime. It's a runtime that you can
[00:02:38] runtime. It's a runtime that you can embed<00:02:39.120> in<00:02:39.280> your<00:02:39.440> Rust<00:02:39.760> application,<00:02:40.800> allowing
[00:02:41] embed in your Rust application, allowing
[00:02:41] embed in your Rust application, allowing you<00:02:41.360> to<00:02:41.519> run<00:02:41.760> Steel<00:02:42.160> code<00:02:42.480> inside<00:02:42.800> your<00:02:42.959> Rust
[00:02:43] you to run Steel code inside your Rust
[00:02:43] you to run Steel code inside your Rust application<00:02:43.760> in<00:02:44.080> kind<00:02:44.160> of<00:02:44.239> a<00:02:44.400> sandbox
[00:02:44] application in kind of a sandbox
[00:02:44] application in kind of a sandbox environment.<00:02:45.920> It's<00:02:46.160> exactly<00:02:46.480> what's<00:02:46.800> needed
[00:02:47] environment. It's exactly what's needed
[00:02:47] environment. It's exactly what's needed for<00:02:47.280> Helix<00:02:47.680> plugins.<00:02:48.560> Steel<00:02:48.959> is<00:02:49.120> a<00:02:49.280> dialect<00:02:49.760> of
[00:02:49] for Helix plugins. Steel is a dialect of
[00:02:50] for Helix plugins. Steel is a dialect of Scheme,<00:02:50.560> which<00:02:50.800> is<00:02:50.959> a<00:02:51.120> dialect<00:02:51.599> of<00:02:51.840> lisp.<00:02:52.400> And
[00:02:52] Scheme, which is a dialect of lisp. And
[00:02:52] Scheme, which is a dialect of lisp. And don't<00:02:52.879> don't<00:02:53.200> hang<00:02:53.360> on,<00:02:53.599> let<00:02:53.760> me<00:02:53.920> explain.<00:02:54.400> A
[00:02:54] don't don't hang on, let me explain. A
[00:02:54] don't don't hang on, let me explain. A lot<00:02:54.720> of<00:02:55.200> developers<00:02:55.680> have<00:02:55.920> a<00:02:56.080> visceral
[00:02:56] lot of developers have a visceral
[00:02:56] lot of developers have a visceral reaction<00:02:56.959> to<00:02:57.360> lisp<00:02:57.760> based<00:02:58.000> languages.<00:02:58.800> You
[00:02:58] reaction to lisp based languages. You
[00:02:58] reaction to lisp based languages. You see<00:02:59.040> all<00:02:59.200> these<00:02:59.440> parentheses<00:02:59.920> and<00:03:00.080> you<00:03:00.239> just
[00:03:00] see all these parentheses and you just
[00:03:00] see all these parentheses and you just want<00:03:00.560> to<00:03:00.720> run.<00:03:00.959> I<00:03:01.200> get<00:03:01.280> it.<00:03:01.519> I've<00:03:01.760> been<00:03:01.920> there.
[00:03:02] want to run. I get it. I've been there.
[00:03:02] want to run. I get it. I've been there. I've<00:03:02.800> had<00:03:02.879> that<00:03:03.040> visceral<00:03:03.440> reaction.<00:03:04.239> Once
[00:03:04] I've had that visceral reaction. Once
[00:03:04] I've had that visceral reaction. Once you<00:03:04.720> understand<00:03:05.120> how<00:03:05.519> simple<00:03:05.760> these
[00:03:06] you understand how simple these
[00:03:06] you understand how simple these languages<00:03:06.560> are,<00:03:07.519> that<00:03:07.840> reaction<00:03:08.239> kind<00:03:08.480> of
[00:03:08] languages are, that reaction kind of
[00:03:08] languages are, that reaction kind of goes<00:03:08.800> away.<00:03:09.360> Just<00:03:09.599> just<00:03:09.840> let<00:03:10.000> me<00:03:10.159> let<00:03:10.319> me<00:03:10.480> show
[00:03:10] goes away. Just just let me let me show
[00:03:10] goes away. Just just let me let me show you.<00:03:10.800> So,<00:03:10.959> currently<00:03:11.280> when<00:03:11.519> you<00:03:11.599> install<00:03:11.920> the
[00:03:12] you. So, currently when you install the
[00:03:12] you. So, currently when you install the plug-in<00:03:12.800> enabled<00:03:13.200> version<00:03:13.360> of<00:03:13.519> Helix,<00:03:14.080> you<00:03:14.319> do
[00:03:14] plug-in enabled version of Helix, you do
[00:03:14] plug-in enabled version of Helix, you do get<00:03:14.720> the<00:03:15.040> steel<00:03:15.519> CLI,<00:03:16.080> the<00:03:16.239> steel<00:03:16.640> ripple<00:03:17.040> with
[00:03:17] get the steel CLI, the steel ripple with
[00:03:17] get the steel CLI, the steel ripple with that.<00:03:17.440> So,<00:03:17.599> I<00:03:17.760> can<00:03:17.840> just<00:03:18.000> do<00:03:18.239> steel<00:03:19.040> and<00:03:19.360> I<00:03:19.599> have
[00:03:19] that. So, I can just do steel and I have
[00:03:19] that. So, I can just do steel and I have a<00:03:19.920> ripple.<00:03:20.400> Now,<00:03:20.560> we're<00:03:20.720> going<00:03:20.800> to<00:03:20.879> easy<00:03:21.120> into
[00:03:21] a ripple. Now, we're going to easy into
[00:03:21] a ripple. Now, we're going to easy into the<00:03:21.440> parentheses<00:03:21.840> here.<00:03:22.000> We're<00:03:22.159> going<00:03:22.239> to
[00:03:22] the parentheses here. We're going to
[00:03:22] the parentheses here. We're going to start<00:03:22.400> with<00:03:22.640> two<00:03:22.800> parenthesis.<00:03:23.360> So,<00:03:23.440> I'm
[00:03:23] start with two parenthesis. So, I'm
[00:03:23] start with two parenthesis. So, I'm going<00:03:23.680> to<00:03:23.760> do<00:03:23.920> open<00:03:24.239> parsn.<00:03:24.720> And<00:03:24.879> when<00:03:25.120> you<00:03:25.200> see
[00:03:25] going to do open parsn. And when you see
[00:03:25] going to do open parsn. And when you see a<00:03:25.760> pair<00:03:25.920> of<00:03:26.080> parenthesis,<00:03:26.879> the<00:03:27.120> inside<00:03:27.440> of
[00:03:27] a pair of parenthesis, the inside of
[00:03:27] a pair of parenthesis, the inside of that<00:03:27.760> parenthesis<00:03:28.239> is<00:03:28.560> usually<00:03:28.879> what's
[00:03:29] that parenthesis is usually what's
[00:03:29] that parenthesis is usually what's called<00:03:29.280> an<00:03:29.599> s<00:03:29.840> expression.<00:03:30.319> And<00:03:30.560> the<00:03:30.799> first
[00:03:31] called an s expression. And the first
[00:03:31] called an s expression. And the first element<00:03:31.360> of<00:03:31.599> that<00:03:32.080> S<00:03:32.319> expression<00:03:33.120> is<00:03:33.360> a
[00:03:33] element of that S expression is a
[00:03:33] element of that S expression is a function<00:03:34.000> name<00:03:34.239> typically.<00:03:34.640> So<00:03:34.720> in<00:03:34.959> this<00:03:35.040> case
[00:03:35] function name typically. So in this case
[00:03:35] function name typically. So in this case that's<00:03:35.440> going<00:03:35.519> to<00:03:35.680> be<00:03:35.840> plus.<00:03:36.640> Plus<00:03:36.879> is<00:03:37.120> a
[00:03:37] that's going to be plus. Plus is a
[00:03:37] that's going to be plus. Plus is a function<00:03:37.519> that<00:03:37.840> adds<00:03:38.159> two<00:03:38.400> numbers.<00:03:39.280> The
[00:03:39] function that adds two numbers. The
[00:03:39] function that adds two numbers. The remainder<00:03:39.920> of<00:03:40.080> that<00:03:40.319> s<00:03:40.480> expression<00:03:40.959> is<00:03:41.280> going
[00:03:41] remainder of that s expression is going
[00:03:41] remainder of that s expression is going to<00:03:41.519> be<00:03:41.760> arguments<00:03:42.239> to<00:03:42.480> that<00:03:42.640> function.<00:03:43.040> So<00:03:43.200> I'm
[00:03:43] to be arguments to that function. So I'm
[00:03:43] to be arguments to that function. So I'm going<00:03:43.440> to<00:03:43.519> do<00:03:43.840> one<00:03:44.400> and<00:03:44.720> two.<00:03:45.120> Those<00:03:45.360> are<00:03:45.440> the
[00:03:45] going to do one and two. Those are the
[00:03:45] going to do one and two. Those are the two<00:03:45.920> arguments<00:03:46.319> that<00:03:46.480> I'm<00:03:46.720> passing<00:03:46.959> to<00:03:47.120> the
[00:03:47] two arguments that I'm passing to the
[00:03:47] two arguments that I'm passing to the plus<00:03:47.519> function.<00:03:48.480> Enter.<00:03:48.879> And<00:03:49.040> that<00:03:49.200> evaluates
[00:03:49] plus function. Enter. And that evaluates
[00:03:49] plus function. Enter. And that evaluates to<00:03:50.000> three.<00:03:50.879> Nice.<00:03:51.360> So<00:03:51.519> that<00:03:51.760> is<00:03:52.480> basic<00:03:53.120> basic
[00:03:53] to three. Nice. So that is basic basic
[00:03:53] to three. Nice. So that is basic basic steel.<00:03:54.400> I<00:03:54.640> can<00:03:54.720> also<00:03:54.959> define<00:03:55.280> a<00:03:55.440> function
[00:03:55] steel. I can also define a function
[00:03:55] steel. I can also define a function using<00:03:56.000> the<00:03:56.159> define<00:03:56.480> macro.<00:03:56.799> So<00:03:56.959> I<00:03:57.200> can<00:03:57.280> do<00:03:57.680> open
[00:03:58] using the define macro. So I can do open
[00:03:58] using the define macro. So I can do open define.<00:03:59.120> The<00:03:59.360> first<00:03:59.519> argument<00:03:59.920> to<00:04:00.080> the<00:04:00.319> define
[00:04:00] define. The first argument to the define
[00:04:00] define. The first argument to the define macro<00:04:01.120> is<00:04:01.519> the<00:04:01.840> name<00:04:02.000> of<00:04:02.159> the<00:04:02.400> function
[00:04:03] macro is the name of the function
[00:04:03] macro is the name of the function followed<00:04:03.519> by<00:04:03.680> the<00:04:03.920> parameters<00:04:04.319> that<00:04:04.560> function
[00:04:04] followed by the parameters that function
[00:04:04] followed by the parameters that function takes.<00:04:05.360> So<00:04:05.439> I<00:04:05.680> can<00:04:05.760> do<00:04:06.319> double<00:04:06.799> and<00:04:06.959> then<00:04:07.200> x.
[00:04:07] takes. So I can do double and then x.
[00:04:07] takes. So I can do double and then x. Double<00:04:07.920> is<00:04:08.000> the<00:04:08.159> name<00:04:08.239> of<00:04:08.319> the<00:04:08.480> function.<00:04:09.200> X<00:04:09.519> is
[00:04:09] Double is the name of the function. X is
[00:04:09] Double is the name of the function. X is the<00:04:10.000> only<00:04:10.159> parameter<00:04:10.480> that<00:04:10.720> the<00:04:10.879> function<00:04:11.040> is
[00:04:11] the only parameter that the function is
[00:04:11] the only parameter that the function is going<00:04:11.360> to<00:04:11.439> take.<00:04:12.159> Close<00:04:12.480> print<00:04:12.720> and<00:04:12.959> then<00:04:13.040> the
[00:04:13] going to take. Close print and then the
[00:04:13] going to take. Close print and then the second<00:04:13.760> argument<00:04:14.159> that<00:04:14.319> I'm<00:04:14.560> passing<00:04:14.799> to<00:04:14.879> the
[00:04:15] second argument that I'm passing to the
[00:04:15] second argument that I'm passing to the defined<00:04:15.360> macro<00:04:15.760> is<00:04:16.239> the<00:04:16.560> body<00:04:16.799> of<00:04:16.959> the
[00:04:17] defined macro is the body of the
[00:04:17] defined macro is the body of the function.<00:04:17.600> So<00:04:17.840> open<00:04:18.239> print<00:04:19.040> multiply<00:04:20.320> to<00:04:20.799> and
[00:04:21] function. So open print multiply to and
[00:04:21] function. So open print multiply to and then<00:04:21.199> x<00:04:22.079> close<00:04:22.400> pen<00:04:22.720> close<00:04:23.040> print.<00:04:23.680> Boom.<00:04:24.160> Now
[00:04:24] then x close pen close print. Boom. Now
[00:04:24] then x close pen close print. Boom. Now I've<00:04:24.479> defined<00:04:24.800> my<00:04:25.040> function<00:04:25.280> and<00:04:25.520> I<00:04:25.680> can<00:04:25.840> do
[00:04:26] I've defined my function and I can do
[00:04:26] I've defined my function and I can do double<00:04:27.199> five<00:04:27.759> which<00:04:28.000> outputs<00:04:28.720> 10.<00:04:29.759> That<00:04:30.000> is
[00:04:30] double five which outputs 10. That is
[00:04:30] double five which outputs 10. That is basic<00:04:30.639> basic<00:04:31.040> steel.<00:04:31.520> But<00:04:31.600> if<00:04:31.840> you<00:04:32.000> understand
[00:04:32] basic basic steel. But if you understand
[00:04:32] basic basic steel. But if you understand this,<00:04:33.520> you<00:04:33.840> understand<00:04:34.320> honestly<00:04:34.720> most<00:04:34.960> of
[00:04:35] this, you understand honestly most of
[00:04:35] this, you understand honestly most of the<00:04:35.280> language.<00:04:35.759> There<00:04:35.919> are<00:04:36.080> some<00:04:36.400> nuances
[00:04:36] the language. There are some nuances
[00:04:36] the language. There are some nuances around<00:04:37.199> defining<00:04:37.600> local<00:04:37.919> variables,<00:04:38.880> macros,
[00:04:39] around defining local variables, macros,
[00:04:39] around defining local variables, macros, these<00:04:39.600> sorts<00:04:39.840> of<00:04:40.000> things.<00:04:40.320> But<00:04:40.560> you<00:04:40.800> basically
[00:04:41] these sorts of things. But you basically
[00:04:41] these sorts of things. But you basically know<00:04:41.520> enough<00:04:41.840> to<00:04:42.080> write<00:04:42.320> plugins<00:04:42.720> at<00:04:42.880> this
[00:04:43] know enough to write plugins at this
[00:04:43] know enough to write plugins at this point.<00:04:43.440> Also,<00:04:43.759> when<00:04:43.919> you<00:04:44.000> install<00:04:44.320> the
[00:04:44] point. Also, when you install the
[00:04:44] point. Also, when you install the plug-in<00:04:44.880> enabled<00:04:45.280> version<00:04:45.440> of<00:04:45.520> Helix
[00:04:45] plug-in enabled version of Helix
[00:04:46] plug-in enabled version of Helix currently,<00:04:46.639> you<00:04:46.880> also<00:04:47.199> get<00:04:47.440> the<00:04:47.919> package
[00:04:48] currently, you also get the package
[00:04:48] currently, you also get the package manager<00:04:48.639> for<00:04:48.880> Steel<00:04:49.600> out<00:04:49.759> of<00:04:49.919> the<00:04:50.000> box.<00:04:50.320> So
[00:04:50] manager for Steel out of the box. So
[00:04:50] manager for Steel out of the box. So that<00:04:50.800> package<00:04:51.199> manager<00:04:51.520> is<00:04:51.680> called<00:04:52.000> forge.
[00:04:52] that package manager is called forge.
[00:04:52] that package manager is called forge. So,<00:04:52.400> I<00:04:52.560> can<00:04:52.639> do<00:04:52.880> forgepkg<00:04:54.160> install--get
[00:04:56] So, I can do forgepkg install--get
[00:04:56] So, I can do forgepkg install--get specify<00:04:56.720> a<00:04:56.880> git<00:04:57.199> repository.<00:04:57.840> I'm<00:04:58.000> going<00:04:58.080> to
[00:04:58] specify a git repository. I'm going to
[00:04:58] specify a git repository. I'm going to grab<00:04:59.120> um<00:04:59.360> this<00:04:59.759> plugin<00:05:00.080> that<00:05:00.240> we're<00:05:00.400> going<00:05:00.400> to
[00:05:00] grab um this plugin that we're going to
[00:05:00] grab um this plugin that we're going to talk<00:05:00.639> about<00:05:00.720> in<00:05:00.880> a<00:05:01.040> second.<00:05:01.600> Specify<00:05:02.000> the
[00:05:02] talk about in a second. Specify the
[00:05:02] talk about in a second. Specify the repository<00:05:02.639> URL.<00:05:03.520> Boom.<00:05:03.919> It's<00:05:04.080> going<00:05:04.240> to
[00:05:04] repository URL. Boom. It's going to
[00:05:04] repository URL. Boom. It's going to install<00:05:04.639> that<00:05:04.800> in<00:05:05.040> a<00:05:05.280> local<00:05:05.680> package
[00:05:06] install that in a local package
[00:05:06] install that in a local package repository.<00:05:06.960> By<00:05:07.199> default,<00:05:07.520> that<00:05:07.759> repository
[00:05:08] repository. By default, that repository
[00:05:08] repository. By default, that repository is<00:05:08.560> at<00:05:09.280> home<00:05:10.240> local<00:05:11.280> share<00:05:12.080> steel<00:05:13.039> cogs.<00:05:13.440> And<00:05:13.600> I
[00:05:13] is at home local share steel cogs. And I
[00:05:13] is at home local share steel cogs. And I think<00:05:13.840> cogs<00:05:14.240> is<00:05:14.400> maybe<00:05:14.560> the<00:05:14.800> working<00:05:15.039> name<00:05:15.360> for
[00:05:16] think cogs is maybe the working name for
[00:05:16] think cogs is maybe the working name for a<00:05:16.240> steel<00:05:16.560> package.<00:05:16.960> I'm<00:05:17.120> not<00:05:17.280> really<00:05:17.440> sure.
[00:05:17] a steel package. I'm not really sure.
[00:05:17] a steel package. I'm not really sure. And<00:05:17.919> I<00:05:18.080> can<00:05:18.240> see<00:05:18.400> I<00:05:18.639> have<00:05:18.800> my<00:05:18.960> oil<00:05:19.280> plugin<00:05:19.600> or<00:05:19.919> my
[00:05:20] And I can see I have my oil plugin or my
[00:05:20] And I can see I have my oil plugin or my oil<00:05:20.479> package<00:05:20.880> in<00:05:21.199> that<00:05:21.360> directory.<00:05:22.080> We'll
[00:05:22] oil package in that directory. We'll
[00:05:22] oil package in that directory. We'll come<00:05:22.479> back<00:05:22.639> back<00:05:22.800> to<00:05:22.880> that<00:05:23.039> in<00:05:23.199> a<00:05:23.360> second.<00:05:23.759> This
[00:05:23] come back back to that in a second. This
[00:05:23] come back back to that in a second. This is<00:05:24.080> one<00:05:24.320> of<00:05:24.400> the<00:05:24.560> directories<00:05:25.039> that<00:05:25.680> Helix
[00:05:26] is one of the directories that Helix
[00:05:26] is one of the directories that Helix looks<00:05:26.479> for<00:05:26.880> when<00:05:27.199> I'm<00:05:27.759> pulling<00:05:28.080> in<00:05:28.240> plugins.
[00:05:28] looks for when I'm pulling in plugins.
[00:05:28] looks for when I'm pulling in plugins. We<00:05:28.880> we'll<00:05:29.039> see<00:05:29.120> what<00:05:29.280> that<00:05:29.440> looks<00:05:29.520> like<00:05:29.680> in<00:05:29.840> a
[00:05:29] We we'll see what that looks like in a
[00:05:30] We we'll see what that looks like in a second.<00:05:30.400> Now,<00:05:30.560> let's<00:05:30.800> talk<00:05:30.960> about<00:05:31.120> what<00:05:31.280> this
[00:05:31] second. Now, let's talk about what this
[00:05:31] second. Now, let's talk about what this looks<00:05:31.600> like<00:05:31.759> from<00:05:31.919> the<00:05:32.080> Helix<00:05:32.479> perspective.
[00:05:32] looks like from the Helix perspective.
[00:05:32] looks like from the Helix perspective. So,<00:05:33.039> I'm<00:05:33.199> going<00:05:33.280> to<00:05:33.360> pop<00:05:33.520> up<00:05:33.600> in<00:05:33.759> Helix.<00:05:34.639> And<00:05:35.039> if
[00:05:35] So, I'm going to pop up in Helix. And if
[00:05:35] So, I'm going to pop up in Helix. And if you've<00:05:35.600> used<00:05:35.759> Helix<00:05:36.160> at<00:05:36.320> all,<00:05:36.479> you're
[00:05:36] you've used Helix at all, you're
[00:05:36] you've used Helix at all, you're probably<00:05:36.960> familiar<00:05:37.280> with<00:05:37.440> this<00:05:37.600> config<00:05:38.080> toml.
[00:05:38] probably familiar with this config toml.
[00:05:38] probably familiar with this config toml. This<00:05:38.960> is<00:05:39.039> a<00:05:39.280> nice<00:05:39.520> configuration<00:05:40.080> file,<00:05:40.479> very
[00:05:40] This is a nice configuration file, very
[00:05:40] This is a nice configuration file, very clean.<00:05:41.440> I<00:05:41.680> can<00:05:41.759> specify<00:05:42.160> key<00:05:42.400> bindings<00:05:42.720> in
[00:05:42] clean. I can specify key bindings in
[00:05:42] clean. I can specify key bindings in here.<00:05:42.960> Here<00:05:43.120> I<00:05:43.280> can<00:05:43.440> specify<00:05:43.759> various<00:05:44.080> tuning
[00:05:44] here. Here I can specify various tuning
[00:05:44] here. Here I can specify various tuning knobs<00:05:44.639> for<00:05:44.800> helix<00:05:45.280> like<00:05:45.840> the<00:05:46.080> type<00:05:46.240> of<00:05:46.400> line
[00:05:46] knobs for helix like the type of line
[00:05:46] knobs for helix like the type of line numbers,<00:05:47.199> whether<00:05:47.440> I<00:05:47.680> want<00:05:47.919> di<00:05:48.240> end<00:05:48.400> of<00:05:48.560> line
[00:05:48] numbers, whether I want di end of line
[00:05:48] numbers, whether I want di end of line diagnostics,<00:05:49.680> what<00:05:49.919> shape<00:05:50.160> I<00:05:50.320> want<00:05:50.479> my
[00:05:50] diagnostics, what shape I want my
[00:05:50] diagnostics, what shape I want my cursor,<00:05:51.120> things<00:05:51.280> like<00:05:51.440> this.<00:05:51.759> This<00:05:52.160> TOML<00:05:52.560> file
[00:05:52] cursor, things like this. This TOML file
[00:05:52] cursor, things like this. This TOML file is<00:05:53.039> still<00:05:53.360> relevant.<00:05:54.000> It<00:05:54.160> is<00:05:54.320> not<00:05:54.479> deprecated
[00:05:55] is still relevant. It is not deprecated
[00:05:55] is still relevant. It is not deprecated or<00:05:55.280> anything<00:05:55.520> like<00:05:55.680> that.<00:05:56.080> You<00:05:56.320> can<00:05:56.479> replace
[00:05:56] or anything like that. You can replace
[00:05:56] or anything like that. You can replace some<00:05:57.039> aspects<00:05:57.360> of<00:05:57.520> it<00:05:57.680> with<00:05:57.840> steel<00:05:58.160> if<00:05:58.320> you
[00:05:58] some aspects of it with steel if you
[00:05:58] some aspects of it with steel if you want<00:05:58.560> to,<00:05:59.039> but<00:05:59.280> there<00:05:59.440> are<00:05:59.600> two<00:05:59.919> new<00:06:00.160> files<00:06:00.720> in
[00:06:00] want to, but there are two new files in
[00:06:00] want to, but there are two new files in the<00:06:01.120> same<00:06:01.360> directory<00:06:01.759> as<00:06:02.000> config<00:06:02.400> toml<00:06:02.960> that
[00:06:03] the same directory as config toml that
[00:06:03] the same directory as config toml that are<00:06:03.440> relevant<00:06:03.840> now<00:06:04.000> that<00:06:04.240> we<00:06:04.400> have<00:06:04.639> steel<00:06:05.039> and
[00:06:05] are relevant now that we have steel and
[00:06:05] are relevant now that we have steel and plugins.<00:06:06.000> The<00:06:06.240> first<00:06:06.400> is<00:06:06.720> helix.<00:06:07.919> SCM.<00:06:08.639> And<00:06:08.800> by
[00:06:08] plugins. The first is helix. SCM. And by
[00:06:08] plugins. The first is helix. SCM. And by the<00:06:09.039> way,<00:06:09.280> SCM<00:06:09.840> stands<00:06:10.080> for<00:06:10.319> scheme.<00:06:11.120> It's<00:06:11.360> not
[00:06:11] the way, SCM stands for scheme. It's not
[00:06:11] the way, SCM stands for scheme. It's not steel.<00:06:12.240> Apparently,<00:06:12.639> we're<00:06:12.960> reusing<00:06:13.440> the
[00:06:13] steel. Apparently, we're reusing the
[00:06:13] steel. Apparently, we're reusing the file<00:06:13.919> extension<00:06:14.319> here,<00:06:14.560> but<00:06:15.280> SCM<00:06:15.680> in<00:06:15.840> the
[00:06:15] file extension here, but SCM in the
[00:06:16] file extension here, but SCM in the context<00:06:16.240> of<00:06:16.400> Helix<00:06:16.880> means<00:06:17.440> steel.<00:06:17.919> So,<00:06:18.080> I<00:06:18.319> have
[00:06:18] context of Helix means steel. So, I have
[00:06:18] context of Helix means steel. So, I have some<00:06:18.639> stuff<00:06:18.800> defined<00:06:19.120> in<00:06:19.280> here.<00:06:19.520> I<00:06:19.759> can<00:06:19.840> define
[00:06:20] some stuff defined in here. I can define
[00:06:20] some stuff defined in here. I can define functions<00:06:20.639> in<00:06:20.800> here<00:06:20.960> if<00:06:21.199> I<00:06:21.360> want<00:06:21.440> to.<00:06:22.080> But<00:06:22.240> the
[00:06:22] functions in here if I want to. But the
[00:06:22] functions in here if I want to. But the more<00:06:22.720> idiomatic<00:06:23.280> way<00:06:23.440> to<00:06:24.000> incorporate
[00:06:24] more idiomatic way to incorporate
[00:06:24] more idiomatic way to incorporate plugins,<00:06:25.199> whether<00:06:25.520> they're<00:06:25.840> communitybuilt
[00:06:26] plugins, whether they're communitybuilt
[00:06:26] plugins, whether they're communitybuilt or<00:06:27.440> custom<00:06:28.000> is<00:06:28.880> init.<00:06:29.600> SCM.<00:06:30.319> And<00:06:30.639> this<00:06:30.880> is<00:06:31.039> also
[00:06:31] or custom is init. SCM. And this is also
[00:06:31] or custom is init. SCM. And this is also a<00:06:31.680> steel<00:06:32.080> file.<00:06:32.800> You<00:06:32.960> can<00:06:33.120> see<00:06:33.199> at<00:06:33.360> the<00:06:33.520> top
[00:06:33] a steel file. You can see at the top
[00:06:33] a steel file. You can see at the top here,<00:06:33.840> I<00:06:34.000> have<00:06:34.160> some<00:06:34.319> stuff.<00:06:34.560> I'm<00:06:34.720> defining<00:06:35.120> a
[00:06:35] here, I have some stuff. I'm defining a
[00:06:35] here, I have some stuff. I'm defining a new<00:06:35.840> LSP.<00:06:36.880> But<00:06:36.960> you<00:06:37.120> can<00:06:37.280> see<00:06:37.440> I'm<00:06:37.680> pulling<00:06:37.919> in
[00:06:38] new LSP. But you can see I'm pulling in
[00:06:38] new LSP. But you can see I'm pulling in my<00:06:38.319> oil<00:06:38.639> plugin<00:06:39.120> here.<00:06:39.440> I'm<00:06:39.759> doing<00:06:40.080> require
[00:06:40] my oil plugin here. I'm doing require
[00:06:40] my oil plugin here. I'm doing require oil/<00:06:41.680> oil.secm.<00:06:43.039> And<00:06:43.280> again,<00:06:44.160> the<00:06:44.479> oil<00:06:44.800> that
[00:06:45] oil/ oil.secm. And again, the oil that
[00:06:45] oil/ oil.secm. And again, the oil that cogs<00:06:45.520> directory<00:06:46.080> is<00:06:46.400> in<00:06:46.720> the<00:06:46.880> path<00:06:47.280> that<00:06:47.600> it's
[00:06:47] cogs directory is in the path that it's
[00:06:47] cogs directory is in the path that it's going<00:06:47.919> to<00:06:48.080> search<00:06:48.720> when<00:06:48.960> it's<00:06:49.280> trying<00:06:49.440> to
[00:06:49] going to search when it's trying to
[00:06:49] going to search when it's trying to evaluate<00:06:50.400> my<00:06:50.720> require<00:06:51.120> statements.<00:06:51.919> This<00:06:52.080> is
[00:06:52] evaluate my require statements. This is
[00:06:52] evaluate my require statements. This is a<00:06:52.639> communitybuilt<00:06:53.360> plugin.<00:06:53.919> And<00:06:54.080> you<00:06:54.240> can<00:06:54.319> see
[00:06:54] a communitybuilt plugin. And you can see
[00:06:54] a communitybuilt plugin. And you can see I<00:06:54.639> have<00:06:54.720> some<00:06:54.880> more<00:06:55.039> invocations<00:06:55.600> of<00:06:55.759> require
[00:06:56] I have some more invocations of require
[00:06:56] I have some more invocations of require down<00:06:56.400> here.<00:06:56.880> These<00:06:57.120> are<00:06:57.280> for<00:06:57.440> my<00:06:57.759> custom
[00:06:58] down here. These are for my custom
[00:06:58] down here. These are for my custom plugins<00:06:58.479> that<00:06:58.639> I've<00:06:58.880> built.<00:06:59.759> These<00:07:00.080> plugins
[00:07:00] plugins that I've built. These plugins
[00:07:00] plugins that I've built. These plugins exist<00:07:01.039> in<00:07:01.280> the<00:07:01.759> Helix<00:07:02.160> config<00:07:02.560> directory<00:07:02.880> as
[00:07:03] exist in the Helix config directory as
[00:07:03] exist in the Helix config directory as well.<00:07:03.759> So<00:07:04.000> that's<00:07:04.240> on<00:07:04.479> the<00:07:04.639> path<00:07:04.800> that<00:07:05.120> it's
[00:07:05] well. So that's on the path that it's
[00:07:05] well. So that's on the path that it's going<00:07:05.520> to<00:07:05.680> search<00:07:06.000> as<00:07:06.160> well.<00:07:06.560> So<00:07:06.720> now<00:07:06.880> let's
[00:07:07] going to search as well. So now let's
[00:07:07] going to search as well. So now let's talk<00:07:07.360> about<00:07:07.680> plug-in<00:07:08.240> development.<00:07:08.960> I'm
[00:07:09] talk about plug-in development. I'm
[00:07:09] talk about plug-in development. I'm going<00:07:09.280> to<00:07:09.360> show<00:07:09.440> you<00:07:09.599> an<00:07:10.000> ultra<00:07:10.400> simple
[00:07:10] going to show you an ultra simple
[00:07:10] going to show you an ultra simple plugin.<00:07:11.520> This<00:07:11.680> is<00:07:11.840> the<00:07:12.160> simplest<00:07:12.639> plugin
[00:07:13] plugin. This is the simplest plugin
[00:07:13] plugin. This is the simplest plugin ever.<00:07:14.000> I'm<00:07:14.240> defining<00:07:14.560> a<00:07:14.800> function<00:07:15.199> insert
[00:07:15] ever. I'm defining a function insert
[00:07:15] ever. I'm defining a function insert hello<00:07:16.400> which<00:07:16.720> just<00:07:17.440> inserts<00:07:18.000> hello<00:07:18.319> world<00:07:18.639> at
[00:07:18] hello which just inserts hello world at
[00:07:18] hello which just inserts hello world at the<00:07:19.039> cursor<00:07:19.440> location.<00:07:20.400> All<00:07:20.560> I<00:07:20.720> have<00:07:20.880> to<00:07:20.960> do<00:07:21.360> is
[00:07:21] the cursor location. All I have to do is
[00:07:21] the cursor location. All I have to do is define<00:07:22.080> the<00:07:22.319> function,<00:07:23.280> define
[00:07:23] define the function, define
[00:07:23] define the function, define documentation<00:07:24.560> for<00:07:24.800> that<00:07:25.039> function.<00:07:25.680> So<00:07:25.919> I'm
[00:07:26] documentation for that function. So I'm
[00:07:26] documentation for that function. So I'm I<00:07:26.319> have<00:07:26.479> two<00:07:26.639> comments<00:07:27.039> here.<00:07:27.759> At<00:07:27.919> doc<00:07:28.240> is<00:07:28.400> the
[00:07:28] I have two comments here. At doc is the
[00:07:28] I have two comments here. At doc is the first<00:07:28.720> line.<00:07:28.960> The<00:07:29.199> second<00:07:29.360> line<00:07:29.680> is
[00:07:30] first line. The second line is
[00:07:30] first line. The second line is describing<00:07:30.639> what<00:07:30.880> the<00:07:31.039> function<00:07:31.360> actually
[00:07:31] describing what the function actually
[00:07:31] describing what the function actually does.<00:07:32.160> And<00:07:32.319> then<00:07:32.400> at<00:07:32.560> the<00:07:32.720> end<00:07:32.800> here<00:07:32.960> I<00:07:33.120> have
[00:07:33] does. And then at the end here I have
[00:07:33] does. And then at the end here I have this<00:07:33.360> provide<00:07:33.680> insert<00:07:34.080> hello.<00:07:34.400> That's<00:07:34.639> just
[00:07:34] this provide insert hello. That's just
[00:07:34] this provide insert hello. That's just basically<00:07:35.199> exporting<00:07:35.599> the<00:07:35.840> function,<00:07:36.639> making
[00:07:36] basically exporting the function, making
[00:07:36] basically exporting the function, making it<00:07:37.039> available<00:07:37.520> wherever<00:07:37.919> this<00:07:38.160> file<00:07:38.479> is
[00:07:38] it available wherever this file is
[00:07:38] it available wherever this file is required.<00:07:39.440> I<00:07:39.599> already<00:07:39.840> have<00:07:39.919> this<00:07:40.160> as<00:07:40.319> part<00:07:40.400> of
[00:07:40] required. I already have this as part of
[00:07:40] required. I already have this as part of my<00:07:40.639> Helix<00:07:41.039> configuration.<00:07:41.599> So<00:07:41.680> if<00:07:41.840> I<00:07:42.000> do<00:07:42.240> colon
[00:07:43] my Helix configuration. So if I do colon
[00:07:43] my Helix configuration. So if I do colon and<00:07:43.360> insert-hello,
[00:07:45] and insert-hello,
[00:07:45] and insert-hello, I<00:07:45.280> can<00:07:45.440> see<00:07:45.599> my<00:07:45.759> function<00:07:46.080> there<00:07:46.400> along<00:07:46.720> with
[00:07:47] I can see my function there along with
[00:07:47] I can see my function there along with the<00:07:47.280> documentation<00:07:47.919> explaining<00:07:48.400> what<00:07:48.560> it
[00:07:48] the documentation explaining what it
[00:07:48] the documentation explaining what it does.<00:07:49.680> And<00:07:49.840> I<00:07:50.000> can<00:07:50.160> run<00:07:50.319> it.<00:07:51.360> Sure<00:07:51.520> enough,<00:07:51.759> it
[00:07:51] does. And I can run it. Sure enough, it
[00:07:52] does. And I can run it. Sure enough, it inserts<00:07:52.400> hello<00:07:52.639> world<00:07:52.880> at<00:07:53.120> the<00:07:53.199> cursor
[00:07:53] inserts hello world at the cursor
[00:07:53] inserts hello world at the cursor location.<00:07:54.240> Ultra<00:07:54.879> ultra<00:07:55.199> simple<00:07:55.759> Helix
[00:07:56] location. Ultra ultra simple Helix
[00:07:56] location. Ultra ultra simple Helix plugin.<00:07:56.800> You<00:07:56.960> can<00:07:57.039> see<00:07:57.199> up<00:07:57.440> here<00:07:57.599> I'm<00:07:57.840> doing
[00:07:58] plugin. You can see up here I'm doing
[00:07:58] plugin. You can see up here I'm doing require<00:07:58.639> Helix/static.sem.
[00:08:00] require Helix/static.sem.
[00:08:00] require Helix/static.sem. This<00:08:01.199> is<00:08:01.360> one<00:08:01.599> of<00:08:01.840> many<00:08:02.080> files<00:08:02.479> that<00:08:02.720> expose
[00:08:03] This is one of many files that expose
[00:08:03] This is one of many files that expose APIs<00:08:03.840> for<00:08:04.000> plugins<00:08:04.479> to<00:08:04.639> use<00:08:04.800> to<00:08:05.039> do<00:08:05.120> what<00:08:05.280> they
[00:08:05] APIs for plugins to use to do what they
[00:08:05] APIs for plugins to use to do what they need<00:08:05.599> to<00:08:05.680> do.<00:08:06.160> static.scm<00:08:07.199> happens<00:08:07.520> to<00:08:07.680> have
[00:08:07] need to do. static.scm happens to have
[00:08:07] need to do. static.scm happens to have the<00:08:08.080> insert<00:08:08.479> string<00:08:08.720> function.<00:08:09.360> So<00:08:09.520> that's
[00:08:09] the insert string function. So that's
[00:08:09] the insert string function. So that's why<00:08:09.919> I'm<00:08:10.400> requiring<00:08:10.800> it<00:08:11.039> here.<00:08:11.440> You'll<00:08:11.680> notice
[00:08:11] why I'm requiring it here. You'll notice
[00:08:11] why I'm requiring it here. You'll notice though<00:08:12.080> that<00:08:12.319> in<00:08:12.479> this<00:08:12.639> case<00:08:12.879> it's<00:08:13.199> pulling
[00:08:13] though that in this case it's pulling
[00:08:13] though that in this case it's pulling everything<00:08:13.759> in<00:08:14.000> static.<00:08:14.560> SCM<00:08:14.960> into<00:08:15.120> the<00:08:15.280> name
[00:08:15] everything in static. SCM into the name
[00:08:15] everything in static. SCM into the name space<00:08:15.759> that<00:08:15.919> I<00:08:16.080> have<00:08:16.240> here.<00:08:16.879> Insert<00:08:17.360> string
[00:08:18] space that I have here. Insert string
[00:08:18] space that I have here. Insert string might<00:08:18.479> potentially<00:08:18.960> collide<00:08:19.440> with<00:08:19.680> something
[00:08:19] might potentially collide with something
[00:08:19] might potentially collide with something that<00:08:20.160> I<00:08:20.319> have<00:08:20.800> in<00:08:21.039> this<00:08:21.280> file<00:08:21.599> or<00:08:21.919> in<00:08:22.240> something
[00:08:22] that I have in this file or in something
[00:08:22] that I have in this file or in something else<00:08:22.720> that<00:08:22.960> I'm<00:08:23.199> pulling<00:08:23.440> into<00:08:23.680> this<00:08:23.840> file.
[00:08:24] else that I'm pulling into this file.
[00:08:24] else that I'm pulling into this file. Um,<00:08:24.720> so<00:08:24.879> I<00:08:25.039> want<00:08:25.120> to<00:08:25.199> show<00:08:25.280> you<00:08:25.440> a<00:08:25.599> different
[00:08:25] Um, so I want to show you a different
[00:08:25] Um, so I want to show you a different plugin<00:08:26.319> line<00:08:26.639> count<00:08:27.199> where<00:08:27.599> I'm<00:08:27.919> actually
[00:08:28] plugin line count where I'm actually
[00:08:28] plugin line count where I'm actually mitigating<00:08:28.639> this<00:08:28.960> by<00:08:29.360> doing<00:08:29.680> prefix<00:08:30.240> in<00:08:30.479> and
[00:08:30] mitigating this by doing prefix in and
[00:08:30] mitigating this by doing prefix in and then<00:08:31.360> I'm<00:08:31.840> still<00:08:32.240> pointing<00:08:32.479> to
[00:08:32] then I'm still pointing to
[00:08:32] then I'm still pointing to helix/static.sem,
[00:08:34] helix/static.sem,
[00:08:34] helix/static.sem, but<00:08:35.120> I'm<00:08:35.440> saying<00:08:35.680> I<00:08:35.919> want<00:08:36.000> to<00:08:36.159> prefix
[00:08:36] but I'm saying I want to prefix
[00:08:36] but I'm saying I want to prefix everything<00:08:36.880> in<00:08:37.120> that<00:08:37.279> file<00:08:37.519> with
[00:08:37] everything in that file with
[00:08:37] everything in that file with helix.static.<00:08:39.279> So<00:08:39.440> [[every]]<00:08:39.680> time<00:08:39.839> I<00:08:40.000> want<00:08:40.080> to
[00:08:40] helix.static. So every time I want to
[00:08:40] helix.static. So every time I want to use<00:08:40.399> a<00:08:40.640> function<00:08:40.959> from<00:08:41.200> that<00:08:41.360> file,<00:08:41.680> I<00:08:41.919> have<00:08:42.000> to
[00:08:42] use a function from that file, I have to
[00:08:42] use a function from that file, I have to prefix<00:08:42.479> it<00:08:42.560> with<00:08:42.719> helix.static,<00:08:44.000> which
[00:08:44] prefix it with helix.static, which
[00:08:44] prefix it with helix.static, which prevents<00:08:44.640> me<00:08:44.880> from<00:08:45.120> polluting<00:08:45.600> my<00:08:45.760> namespace
[00:08:46] prevents me from polluting my namespace
[00:08:46] prevents me from polluting my namespace in<00:08:46.399> ways<00:08:46.560> that<00:08:46.720> I<00:08:46.880> might<00:08:47.040> not<00:08:47.200> expect.<00:08:47.760> This
[00:08:47] in ways that I might not expect. This
[00:08:48] in ways that I might not expect. This plugin<00:08:48.399> just<00:08:48.560> counts<00:08:48.800> the<00:08:48.959> number<00:08:49.120> of<00:08:49.279> lines
[00:08:49] plugin just counts the number of lines
[00:08:49] plugin just counts the number of lines in<00:08:49.600> the<00:08:49.760> current<00:08:50.000> file.<00:08:50.320> I<00:08:50.560> can<00:08:50.640> do<00:08:51.279> count
[00:08:51] in the current file. I can do count
[00:08:51] in the current file. I can do count lines<00:08:52.560> and<00:08:52.720> it's<00:08:52.880> going<00:08:52.959> to<00:08:53.040> give<00:08:53.200> me<00:08:53.360> 13.<00:08:53.839> Very
[00:08:54] lines and it's going to give me 13. Very
[00:08:54] lines and it's going to give me 13. Very useful,<00:08:54.720> right?<00:08:55.120> I'm<00:08:55.360> also<00:08:55.680> incorporating
[00:08:56] useful, right? I'm also incorporating
[00:08:56] useful, right? I'm also incorporating another<00:08:56.800> file<00:08:57.200> that<00:08:57.440> exposes<00:08:57.920> a<00:08:58.080> again<00:08:58.399> APIs
[00:08:59] another file that exposes a again APIs
[00:08:59] another file that exposes a again APIs that<00:08:59.279> can<00:08:59.440> be<00:08:59.680> leveraged<00:09:00.080> by<00:09:00.240> plugins.
[00:09:01] that can be leveraged by plugins.
[00:09:01] that can be leveraged by plugins. Commands<00:09:01.680> SCM.<00:09:02.480> That's<00:09:02.640> where<00:09:02.800> the<00:09:02.959> echo
[00:09:03] Commands SCM. That's where the echo
[00:09:03] Commands SCM. That's where the echo command<00:09:03.600> is<00:09:03.760> that<00:09:03.920> allows<00:09:04.160> me<00:09:04.320> to<00:09:04.480> echo<00:09:04.800> that
[00:09:05] command is that allows me to echo that
[00:09:05] command is that allows me to echo that text<00:09:05.600> to<00:09:05.920> the<00:09:06.080> Helix<00:09:06.560> status<00:09:06.880> line.<00:09:07.200> I'm<00:09:07.440> not
[00:09:07] text to the Helix status line. I'm not
[00:09:07] text to the Helix status line. I'm not going<00:09:07.600> to<00:09:07.680> go<00:09:07.760> through<00:09:07.920> this<00:09:08.080> plug-in<00:09:08.320> line<00:09:08.560> by
[00:09:08] going to go through this plug-in line by
[00:09:08] going to go through this plug-in line by line.<00:09:09.040> I<00:09:09.200> just<00:09:09.279> want<00:09:09.440> to<00:09:09.519> show<00:09:09.680> you<00:09:09.920> an<00:09:10.240> example
[00:09:10] line. I just want to show you an example
[00:09:10] line. I just want to show you an example of<00:09:10.959> what's<00:09:11.279> possible<00:09:11.680> and<00:09:12.080> kind<00:09:12.240> of<00:09:12.399> patterns
[00:09:12] of what's possible and kind of patterns
[00:09:12] of what's possible and kind of patterns that<00:09:12.880> you<00:09:13.040> can<00:09:13.120> build<00:09:13.360> off<00:09:13.519> of.<00:09:13.839> There<00:09:14.080> are
[00:09:14] that you can build off of. There are
[00:09:14] that you can build off of. There are several<00:09:14.640> more<00:09:14.880> files<00:09:15.200> that<00:09:15.440> you<00:09:15.680> can<00:09:15.839> pull
[00:09:16] several more files that you can pull
[00:09:16] several more files that you can pull into<00:09:16.320> your<00:09:16.640> plugin<00:09:17.120> to<00:09:17.760> get<00:09:18.000> access<00:09:18.320> to<00:09:18.480> more
[00:09:18] into your plugin to get access to more
[00:09:18] into your plugin to get access to more functionality<00:09:19.279> exposed<00:09:19.680> by<00:09:19.839> Helix.<00:09:20.480> I'm
[00:09:20] functionality exposed by Helix. I'm
[00:09:20] functionality exposed by Helix. I'm going<00:09:20.720> to<00:09:20.800> show<00:09:20.880> you<00:09:21.040> one<00:09:21.279> more<00:09:21.440> plugin<00:09:22.160> that's
[00:09:22] going to show you one more plugin that's
[00:09:22] going to show you one more plugin that's readbuffer.sc.<00:09:23.360> SCM.<00:09:24.320> This<00:09:24.560> plugin<00:09:24.959> just
[00:09:25] readbuffer.sc. SCM. This plugin just
[00:09:25] readbuffer.sc. SCM. This plugin just outputs<00:09:25.920> whatever<00:09:26.320> line<00:09:26.560> four<00:09:26.800> of<00:09:26.959> the
[00:09:27] outputs whatever line four of the
[00:09:27] outputs whatever line four of the current<00:09:27.279> buffer<00:09:27.600> is.<00:09:28.880> So<00:09:29.040> if<00:09:29.279> I<00:09:29.440> do<00:09:30.080> read<00:09:30.320> line
[00:09:30] current buffer is. So if I do read line
[00:09:30] current buffer is. So if I do read line four,<00:09:31.760> it's<00:09:32.000> going<00:09:32.080> to<00:09:32.160> output<00:09:33.040> the<00:09:33.279> the<00:09:33.760> doc
[00:09:33] four, it's going to output the the doc
[00:09:34] four, it's going to output the the doc comment<00:09:34.320> there.<00:09:34.640> You<00:09:34.800> never<00:09:34.959> know<00:09:35.040> when
[00:09:35] comment there. You never know when
[00:09:35] comment there. You never know when you're<00:09:35.279> in<00:09:35.440> a<00:09:35.600> buffer.<00:09:35.920> You<00:09:36.080> might<00:09:36.240> need<00:09:36.320> to
[00:09:36] you're in a buffer. You might need to
[00:09:36] you're in a buffer. You might need to suddenly<00:09:37.279> know<00:09:37.440> what<00:09:37.680> line<00:09:38.000> four<00:09:38.240> says<00:09:38.480> when
[00:09:38] suddenly know what line four says when
[00:09:38] suddenly know what line four says when you're,<00:09:38.959> you<00:09:39.040> know,<00:09:39.200> on<00:09:39.360> line<00:09:39.600> 10,000<00:09:40.000> or
[00:09:40] you're, you know, on line 10,000 or
[00:09:40] you're, you know, on line 10,000 or whatever.<00:09:40.480> You<00:09:40.560> never<00:09:40.800> know.<00:09:41.120> I'm<00:09:41.279> not<00:09:41.440> going
[00:09:41] whatever. You never know. I'm not going
[00:09:41] whatever. You never know. I'm not going to<00:09:41.600> go<00:09:41.680> through<00:09:41.760> this<00:09:41.920> line<00:09:42.160> by<00:09:42.399> line,<00:09:42.640> but<00:09:42.880> you
[00:09:43] to go through this line by line, but you
[00:09:43] to go through this line by line, but you can<00:09:43.200> kind<00:09:43.279> of<00:09:43.440> take<00:09:43.600> this<00:09:43.839> pattern.<00:09:44.240> I'm
[00:09:44] can kind of take this pattern. I'm
[00:09:44] can kind of take this pattern. I'm reading<00:09:44.880> arbitrary<00:09:45.360> text<00:09:45.600> from<00:09:45.760> the<00:09:45.920> current
[00:09:46] reading arbitrary text from the current
[00:09:46] reading arbitrary text from the current buffer<00:09:46.800> and<00:09:47.120> doing<00:09:47.360> something<00:09:47.680> useful<00:09:47.920> with
[00:09:48] buffer and doing something useful with
[00:09:48] buffer and doing something useful with it.<00:09:48.320> So<00:09:48.480> if<00:09:48.640> you<00:09:48.720> want<00:09:48.880> to<00:09:48.959> see<00:09:49.040> an<00:09:49.200> exhaustive
[00:09:49] it. So if you want to see an exhaustive
[00:09:49] it. So if you want to see an exhaustive list<00:09:49.839> of<00:09:50.000> all<00:09:50.160> the<00:09:50.240> APIs<00:09:50.720> that<00:09:50.880> are<00:09:51.040> exposed
[00:09:51] list of all the APIs that are exposed
[00:09:51] list of all the APIs that are exposed right<00:09:51.519> now<00:09:51.680> for<00:09:52.000> plugins<00:09:52.320> to<00:09:52.560> use,<00:09:52.800> you<00:09:52.959> can
[00:09:53] right now for plugins to use, you can
[00:09:53] right now for plugins to use, you can look<00:09:53.200> at<00:09:53.279> the<00:09:53.519> steel<00:09:53.839> docs.md<00:09:55.120> in<00:09:55.279> the<00:09:55.440> poll
[00:09:55] look at the steel docs.md in the poll
[00:09:55] look at the steel docs.md in the poll request,<00:09:56.480> it<00:09:56.720> is<00:09:57.200> a<00:09:57.440> pretty<00:09:57.680> exhaustive<00:09:58.240> list
[00:09:58] request, it is a pretty exhaustive list
[00:09:58] request, it is a pretty exhaustive list of<00:09:58.720> all<00:09:58.880> the<00:09:59.120> functions<00:09:59.600> that<00:09:59.839> are<00:10:00.000> made
[00:10:00] of all the functions that are made
[00:10:00] of all the functions that are made available<00:10:00.560> to<00:10:00.800> plugins.<00:10:01.600> Each<00:10:01.839> of<00:10:01.920> the
[00:10:02] available to plugins. Each of the
[00:10:02] available to plugins. Each of the functions<00:10:02.480> has<00:10:02.720> pretty<00:10:02.959> varying<00:10:03.279> levels<00:10:03.600> of
[00:10:03] functions has pretty varying levels of
[00:10:03] functions has pretty varying levels of documentation.<00:10:04.720> Some<00:10:04.959> have<00:10:05.120> pretty<00:10:05.360> minimal
[00:10:05] documentation. Some have pretty minimal
[00:10:05] documentation. Some have pretty minimal documentation,<00:10:06.880> but<00:10:07.360> um<00:10:07.600> you<00:10:07.760> can<00:10:07.920> usually
[00:10:08] documentation, but um you can usually
[00:10:08] documentation, but um you can usually find<00:10:08.399> what<00:10:08.560> you<00:10:08.640> need<00:10:08.800> in<00:10:08.959> here.<00:10:09.200> And<00:10:09.360> if<00:10:09.519> you
[00:10:09] find what you need in here. And if you
[00:10:09] find what you need in here. And if you want,<00:10:09.760> you<00:10:09.920> can<00:10:10.000> feed<00:10:10.240> this<00:10:10.399> into<00:10:10.560> a<00:10:10.720> language
[00:10:11] want, you can feed this into a language
[00:10:11] want, you can feed this into a language model,<00:10:11.760> tell<00:10:12.000> the<00:10:12.160> language<00:10:12.399> model<00:10:12.640> what<00:10:12.880> you
[00:10:12] model, tell the language model what you
[00:10:12] model, tell the language model what you want<00:10:13.040> to<00:10:13.200> do,<00:10:13.360> and<00:10:13.680> it<00:10:13.920> can<00:10:14.079> help<00:10:14.160> you<00:10:14.480> kind<00:10:14.640> of
[00:10:14] want to do, and it can help you kind of
[00:10:14] want to do, and it can help you kind of put<00:10:14.959> together<00:10:15.920> the<00:10:16.160> pieces<00:10:16.480> of<00:10:16.640> functionality
[00:10:17] put together the pieces of functionality
[00:10:17] put together the pieces of functionality that<00:10:17.279> you<00:10:17.440> need<00:10:17.519> to<00:10:17.680> string<00:10:18.000> together<00:10:18.320> to<00:10:18.480> to
[00:10:18] that you need to string together to to
[00:10:18] that you need to string together to to get<00:10:18.880> where<00:10:19.040> you<00:10:19.200> need<00:10:19.279> to<00:10:19.360> be.<00:10:19.839> So,<00:10:20.320> what
[00:10:20] get where you need to be. So, what
[00:10:20] get where you need to be. So, what plugins<00:10:21.120> already<00:10:21.440> exist?<00:10:21.920> Well,<00:10:23.120> not<00:10:23.360> a<00:10:23.600> ton
[00:10:23] plugins already exist? Well, not a ton
[00:10:23] plugins already exist? Well, not a ton because<00:10:24.160> this<00:10:24.560> hasn't<00:10:24.800> even<00:10:24.959> merged<00:10:25.279> yet,<00:10:25.600> but
[00:10:25] because this hasn't even merged yet, but
[00:10:26] because this hasn't even merged yet, but there<00:10:26.399> are<00:10:26.640> two<00:10:26.800> notable<00:10:27.200> plugins<00:10:27.519> I<00:10:27.680> want<00:10:27.760> to
[00:10:27] there are two notable plugins I want to
[00:10:27] there are two notable plugins I want to mention.<00:10:28.160> One<00:10:28.240> is<00:10:28.399> Vim.hx.<00:10:29.440> I<00:10:29.600> feel<00:10:29.760> like
[00:10:29] mention. One is Vim.hx. I feel like
[00:10:29] mention. One is Vim.hx. I feel like Helix<00:10:30.399> would<00:10:30.560> have<00:10:30.720> already<00:10:30.959> kind<00:10:31.120> of<00:10:31.760> grabbed
[00:10:32] Helix would have already kind of grabbed
[00:10:32] Helix would have already kind of grabbed a<00:10:32.240> lot<00:10:32.320> of<00:10:32.399> Vim's<00:10:32.800> market<00:10:32.959> share<00:10:33.200> if<00:10:33.360> it<00:10:33.519> just
[00:10:33] a lot of Vim's market share if it just
[00:10:33] a lot of Vim's market share if it just had<00:10:33.920> a<00:10:34.240> Vim<00:10:34.560> mode.
[00:10:35] had a Vim mode.
[00:10:35] had a Vim mode. &gt;&gt; Well,<00:10:35.600> now<00:10:35.760> it<00:10:36.000> does.<00:10:36.320> It<00:10:36.640> exists<00:10:36.959> now.
[00:10:37] &gt;&gt; Well, now it does. It exists now.
[00:10:37] &gt;&gt; Well, now it does. It exists now. &gt;&gt; It<00:10:37.839> exists.<00:10:38.240> I<00:10:38.480> have<00:10:38.640> it
[00:10:39] &gt;&gt; It exists. I have it
[00:10:40] &gt;&gt; It exists. I have it &gt;&gt; as<00:10:40.240> a<00:10:40.399> plugin<00:10:40.640> or<00:10:40.800> you<00:10:40.959> talking<00:10:41.040> about<00:10:41.200> Evil
[00:10:41] &gt;&gt; as a plugin or you talking about Evil
[00:10:41] &gt;&gt; as a plugin or you talking about Evil Helix?
[00:10:41] Helix?
[00:10:41] Helix? &gt;&gt; As<00:10:42.079> a<00:10:42.240> plugin?
[00:10:43] &gt;&gt; As a plugin?
[00:10:43] &gt;&gt; As a plugin? &gt;&gt; Okay.
[00:10:43] &gt;&gt; Okay.
[00:10:43] &gt;&gt; Okay. &gt;&gt; This<00:10:44.399> exists.<00:10:44.527> [clears throat]
[00:10:45] &gt;&gt; This exists. [clears throat]
[00:10:45] &gt;&gt; This exists. [clears throat] &gt;&gt; It's<00:10:45.279> It's<00:10:45.600> a<00:10:45.760> public<00:10:46.000> repo<00:10:46.320> and<00:10:46.480> it's
[00:10:46] &gt;&gt; It's It's a public repo and it's
[00:10:46] &gt;&gt; It's It's a public repo and it's available.
[00:10:47] available.
[00:10:47] available. &gt;&gt; Yeah,<00:10:47.600> I<00:10:47.760> can<00:10:47.839> send<00:10:47.920> it<00:10:48.079> to<00:10:48.240> you.<00:10:48.399> It's<00:10:48.800> um
[00:10:48] &gt;&gt; Yeah, I can send it to you. It's um
[00:10:48] &gt;&gt; Yeah, I can send it to you. It's um &gt;&gt; Oh,<00:10:49.200> that's<00:10:49.440> incredible.
[00:10:49] &gt;&gt; Oh, that's incredible.
[00:10:50] &gt;&gt; Oh, that's incredible. &gt;&gt; It<00:10:50.240> does<00:10:50.399> exist.<00:10:51.040> I<00:10:51.279> wrote<00:10:51.600> it<00:10:51.760> all.<00:10:52.160> I<00:10:52.320> I
[00:10:52] &gt;&gt; It does exist. I wrote it all. I I
[00:10:52] &gt;&gt; It does exist. I wrote it all. I I touched<00:10:52.880> it.<00:10:53.040> No<00:10:53.279> AI.<00:10:53.760> I<00:10:54.000> wrote<00:10:54.560> maybe<00:10:54.800> like<00:10:55.600> 30
[00:10:55] touched it. No AI. I wrote maybe like 30
[00:10:56] touched it. No AI. I wrote maybe like 30 functions<00:10:56.320> or<00:10:56.560> something<00:10:56.720> and<00:10:56.880> I<00:10:57.040> was<00:10:57.120> just
[00:10:57] functions or something and I was just
[00:10:57] functions or something and I was just like<00:10:57.440> going<00:10:57.600> through<00:10:57.760> the<00:10:57.920> exercise<00:10:58.399> like<00:10:58.640> I
[00:10:58] like going through the exercise like I
[00:10:58] like going through the exercise like I got<00:10:59.120> really<00:10:59.360> far.<00:10:59.680> I<00:10:59.839> was<00:10:59.920> like,<00:11:00.000> "Oh<00:11:00.320> my<00:11:00.399> god,
[00:11:00] got really far. I was like, "Oh my god,
[00:11:00] got really far. I was like, "Oh my god, wait.<00:11:00.880> This<00:11:00.959> is<00:11:01.120> actually<00:11:01.360> possible."
[00:11:02] wait. This is actually possible."
[00:11:02] wait. This is actually possible." &gt;&gt; Okay.
[00:11:02] &gt;&gt; Okay.
[00:11:02] &gt;&gt; Okay. &gt;&gt; I<00:11:02.720> threw<00:11:02.959> it<00:11:03.120> up.<00:11:03.519> Didn't<00:11:03.760> tell<00:11:03.920> a<00:11:04.160> soul.
[00:11:05] &gt;&gt; I threw it up. Didn't tell a soul.
[00:11:05] &gt;&gt; I threw it up. Didn't tell a soul. &gt;&gt; Just<00:11:05.600> put<00:11:05.839> it<00:11:05.920> up.<00:11:06.640> Someone<00:11:07.040> picked<00:11:07.279> it<00:11:07.440> up,
[00:11:07] &gt;&gt; Just put it up. Someone picked it up,
[00:11:08] &gt;&gt; Just put it up. Someone picked it up, threw<00:11:08.240> it<00:11:08.399> at<00:11:08.720> AI,<00:11:09.519> and<00:11:09.839> spit<00:11:10.160> out<00:11:10.720> a<00:11:10.959> couple
[00:11:11] threw it at AI, and spit out a couple
[00:11:11] threw it at AI, and spit out a couple thousand<00:11:11.519> lines,<00:11:11.760> and<00:11:11.920> they<00:11:12.079> like<00:11:12.399> finished
[00:11:12] thousand lines, and they like finished
[00:11:12] thousand lines, and they like finished it.<00:11:13.040> I<00:11:13.200> think<00:11:13.360> it's<00:11:13.519> I<00:11:13.760> think<00:11:13.839> it's<00:11:14.000> not<00:11:14.160> like
[00:11:14] it. I think it's I think it's not like
[00:11:14] it. I think it's I think it's not like 100%<00:11:14.959> accurate<00:11:15.279> to<00:11:15.600> every<00:11:15.760> single<00:11:16.000> thing.<00:11:16.240> Of
[00:11:16] 100% accurate to every single thing. Of
[00:11:16] 100% accurate to every single thing. Of course,<00:11:16.560> like<00:11:16.720> there's<00:11:16.880> going<00:11:16.959> to<00:11:17.040> be<00:11:17.200> bugs.
[00:11:17] course, like there's going to be bugs.
[00:11:17] course, like there's going to be bugs. The<00:11:17.680> surface<00:11:18.000> area<00:11:18.240> of<00:11:18.399> coverage<00:11:18.640> of<00:11:18.720> the<00:11:18.800> Vim
[00:11:19] The surface area of coverage of the Vim
[00:11:19] The surface area of coverage of the Vim emulation<00:11:19.519> is<00:11:20.079> shockingly<00:11:20.720> large.<00:11:21.279> It<00:11:21.440> like
[00:11:21] emulation is shockingly large. It like
[00:11:21] emulation is shockingly large. It like to<00:11:21.839> the<00:11:22.000> point<00:11:22.160> where
[00:11:23] to the point where
[00:11:23] to the point where &gt;&gt; I<00:11:24.160> boot<00:11:24.399> it<00:11:24.560> up<00:11:24.640> and<00:11:24.800> I<00:11:24.880> go,<00:11:25.040> "Oh<00:11:25.200> my<00:11:25.279> god,<00:11:25.519> how
[00:11:25] &gt;&gt; I boot it up and I go, "Oh my god, how
[00:11:25] &gt;&gt; I boot it up and I go, "Oh my god, how do<00:11:25.680> I<00:11:25.839> get<00:11:25.920> back<00:11:26.000> to<00:11:26.079> Helix<00:11:26.480> is<00:11:26.640> freaking<00:11:26.880> me
[00:11:27] do I get back to Helix is freaking me
[00:11:27] do I get back to Helix is freaking me out<00:11:27.519> like<00:11:28.079> cuz<00:11:28.320> I<00:11:28.640> cuz<00:11:28.800> I<00:11:28.959> like<00:11:29.120> do<00:11:29.360> something
[00:11:29] out like cuz I cuz I like do something
[00:11:29] out like cuz I cuz I like do something and<00:11:29.680> it's<00:11:29.839> Vim<00:11:30.160> and<00:11:30.320> I'm<00:11:30.399> like,<00:11:30.480> "Oh<00:11:30.720> my<00:11:30.800> god."
[00:11:31] and it's Vim and I'm like, "Oh my god."
[00:11:31] and it's Vim and I'm like, "Oh my god." The<00:11:31.360> other<00:11:31.519> plugin<00:11:31.839> that<00:11:32.079> actually<00:11:32.399> has<00:11:32.720> some
[00:11:33] The other plugin that actually has some
[00:11:33] The other plugin that actually has some following<00:11:33.519> now<00:11:33.760> at<00:11:34.000> 39<00:11:34.399> stars.<00:11:35.200> Pretty<00:11:35.440> good
[00:11:35] following now at 39 stars. Pretty good
[00:11:35] following now at 39 stars. Pretty good for<00:11:36.560> a<00:11:36.800> plugin<00:11:37.120> for<00:11:37.279> a<00:11:37.440> system<00:11:37.600> that<00:11:37.839> hasn't
[00:11:37] for a plugin for a system that hasn't
[00:11:38] for a plugin for a system that hasn't even<00:11:38.160> merged<00:11:38.480> yet,<00:11:38.720> right?<00:11:39.360> Oil.hx.<00:11:40.320> This<00:11:40.480> is
[00:11:40] even merged yet, right? Oil.hx. This is
[00:11:40] even merged yet, right? Oil.hx. This is basically<00:11:41.040> the<00:11:41.839> Helix<00:11:42.320> equivalent<00:11:42.720> of<00:11:42.880> the
[00:11:43] basically the Helix equivalent of the
[00:11:43] basically the Helix equivalent of the Neoim<00:11:43.600> oil<00:11:43.839> plugin,<00:11:44.160> which<00:11:44.399> I<00:11:44.640> use
[00:11:44] Neoim oil plugin, which I use
[00:11:44] Neoim oil plugin, which I use extensively.<00:11:45.920> It<00:11:46.160> allows<00:11:46.399> you<00:11:46.560> to<00:11:46.720> treat<00:11:47.040> a
[00:11:47] extensively. It allows you to treat a
[00:11:47] extensively. It allows you to treat a directory<00:11:47.680> listing<00:11:48.160> like<00:11:48.399> a<00:11:48.640> buffer.<00:11:49.360> You<00:11:49.519> can
[00:11:49] directory listing like a buffer. You can
[00:11:49] directory listing like a buffer. You can add<00:11:49.920> files<00:11:50.399> by<00:11:50.720> inserting<00:11:51.200> lines,<00:11:51.680> remove
[00:11:51] add files by inserting lines, remove
[00:11:51] add files by inserting lines, remove files<00:11:52.240> by<00:11:52.480> removing<00:11:52.880> lines,<00:11:53.200> rename<00:11:53.600> things,
[00:11:54] files by removing lines, rename things,
[00:11:54] files by removing lines, rename things, all<00:11:54.480> within<00:11:54.880> a<00:11:55.120> text<00:11:55.360> buffer.<00:11:55.839> Very,<00:11:56.079> very
[00:11:56] all within a text buffer. Very, very
[00:11:56] all within a text buffer. Very, very handy.<00:11:56.640> I<00:11:56.800> use<00:11:56.959> that<00:11:57.120> one<00:11:57.279> all<00:11:57.360> the<00:11:57.519> time<00:11:57.600> in
[00:11:57] handy. I use that one all the time in
[00:11:57] handy. I use that one all the time in the<00:11:57.839> Neovim<00:11:58.480> ecosystem.<00:11:59.120> I<00:11:59.360> did<00:11:59.519> want<00:11:59.600> to<00:11:59.760> talk
[00:11:59] the Neovim ecosystem. I did want to talk
[00:11:59] the Neovim ecosystem. I did want to talk about<00:11:59.920> a<00:12:00.160> few<00:12:00.240> of<00:12:00.320> the<00:12:00.399> pieces<00:12:00.640> of<00:12:00.800> drama<00:12:01.120> in
[00:12:01] about a few of the pieces of drama in
[00:12:01] about a few of the pieces of drama in that<00:12:01.440> poll<00:12:01.680> request.<00:12:02.560> Um,<00:12:02.880> this<00:12:03.120> fellow<00:12:03.360> says,
[00:12:03] that poll request. Um, this fellow says,
[00:12:03] that poll request. Um, this fellow says, "Personally,<00:12:03.920> I<00:12:04.160> would<00:12:04.320> consider<00:12:04.640> locking<00:12:04.959> in
[00:12:05] "Personally, I would consider locking in
[00:12:05] "Personally, I would consider locking in a<00:12:05.279> scheme<00:12:05.600> like<00:12:05.760> [[syntax]]<00:12:06.240> as<00:12:06.399> a<00:12:06.560> red<00:12:06.800> flag.<00:12:07.519> For
[00:12:07] a scheme like syntax as a red flag. For
[00:12:07] a scheme like syntax as a red flag. For a<00:12:07.760> plug-in<00:12:08.160> ecosystem<00:12:08.560> to<00:12:08.720> take<00:12:08.880> off,
[00:12:09] a plug-in ecosystem to take off,
[00:12:09] a plug-in ecosystem to take off, motivated<00:12:09.760> individuals<00:12:10.240> would<00:12:10.480> have<00:12:10.639> to
[00:12:10] motivated individuals would have to
[00:12:10] motivated individuals would have to either<00:12:10.959> come<00:12:11.120> from<00:12:11.360> Emacs<00:12:12.240> or<00:12:12.639> overcome<00:12:13.040> the
[00:12:13] either come from Emacs or overcome the
[00:12:13] either come from Emacs or overcome the learning<00:12:13.519> curve<00:12:13.680> of<00:12:13.920> what<00:12:14.000> they're<00:12:14.240> used<00:12:14.399> to.
[00:12:14] learning curve of what they're used to.
[00:12:14] learning curve of what they're used to. Most<00:12:14.880> likely<00:12:15.200> Rust,<00:12:15.600> Vimscript,<00:12:16.160> or<00:12:16.399> Lua.<00:12:16.880> Um,
[00:12:17] Most likely Rust, Vimscript, or Lua. Um,
[00:12:17] Most likely Rust, Vimscript, or Lua. Um, I<00:12:17.279> don't<00:12:17.360> know<00:12:17.440> what<00:12:17.600> the<00:12:17.760> solution<00:12:18.000> is<00:12:18.240> here,
[00:12:18] I don't know what the solution is here,
[00:12:18] I don't know what the solution is here, but<00:12:18.720> this<00:12:18.959> might<00:12:19.120> be<00:12:19.279> something<00:12:19.440> to<00:12:19.600> keep<00:12:19.760> in
[00:12:19] but this might be something to keep in
[00:12:19] but this might be something to keep in mind.<00:12:20.320> Um,<00:12:20.959> yeah,<00:12:21.279> I<00:12:21.519> mean,<00:12:21.600> I<00:12:21.839> think<00:12:22.000> this
[00:12:22] mind. Um, yeah, I mean, I think this
[00:12:22] mind. Um, yeah, I mean, I think this probably<00:12:22.560> resonates<00:12:22.880> with<00:12:23.040> a<00:12:23.200> lot<00:12:23.279> of<00:12:23.360> people.
[00:12:23] probably resonates with a lot of people.
[00:12:23] probably resonates with a lot of people. I<00:12:24.160> think<00:12:24.320> we<00:12:24.480> should<00:12:24.800> kind<00:12:25.040> of<00:12:25.120> come<00:12:25.279> to<00:12:25.440> this
[00:12:25] I think we should kind of come to this
[00:12:25] I think we should kind of come to this with<00:12:25.760> an<00:12:25.920> open<00:12:26.160> mind.<00:12:26.639> I<00:12:26.959> think<00:12:27.200> learning
[00:12:27] with an open mind. I think learning
[00:12:27] with an open mind. I think learning lisps<00:12:28.160> is<00:12:28.720> probably<00:12:29.040> healthy<00:12:29.360> for<00:12:29.600> most
[00:12:29] lisps is probably healthy for most
[00:12:29] lisps is probably healthy for most developers.<00:12:30.560> It's<00:12:30.800> a<00:12:30.959> nook<00:12:31.120> of<00:12:31.279> the
[00:12:31] developers. It's a nook of the
[00:12:31] developers. It's a nook of the [[programming]]<00:12:31.920> ecosystem<00:12:32.320> that<00:12:32.639> kind<00:12:32.720> of<00:12:32.800> gets
[00:12:32] programming ecosystem that kind of gets
[00:12:32] programming ecosystem that kind of gets avoided<00:12:33.360> a<00:12:33.519> lot<00:12:34.320> and<00:12:34.560> it's<00:12:34.800> actually<00:12:35.040> very
[00:12:35] avoided a lot and it's actually very
[00:12:35] avoided a lot and it's actually very very<00:12:35.600> easy<00:12:35.760> to<00:12:36.000> learn<00:12:36.959> basic<00:12:37.600> lisp.<00:12:38.240> I<00:12:38.480> can
[00:12:38] very easy to learn basic lisp. I can
[00:12:38] very easy to learn basic lisp. I can definitely<00:12:38.880> empathize<00:12:39.360> with<00:12:39.600> the<00:12:39.920> oh<00:12:40.079> my
[00:12:40] definitely empathize with the oh my
[00:12:40] definitely empathize with the oh my gosh,<00:12:40.480> there's<00:12:40.720> so<00:12:40.880> many<00:12:41.040> parenthesis,
[00:12:41] gosh, there's so many parenthesis,
[00:12:41] gosh, there's so many parenthesis, right?<00:12:42.160> Like<00:12:42.399> I<00:12:42.560> said,<00:12:42.959> as<00:12:43.200> you<00:12:43.600> learn<00:12:43.839> the
[00:12:44] right? Like I said, as you learn the
[00:12:44] right? Like I said, as you learn the basics<00:12:44.399> of<00:12:44.560> the<00:12:44.720> syntax,<00:12:45.839> you<00:12:46.160> very<00:12:46.480> very
[00:12:46] basics of the syntax, you very very
[00:12:46] basics of the syntax, you very very quickly<00:12:47.120> get<00:12:47.279> over<00:12:47.519> the<00:12:47.680> parenthesis<00:12:48.240> thing.
[00:12:48] quickly get over the parenthesis thing.
[00:12:48] quickly get over the parenthesis thing. The<00:12:48.560> parentheses<00:12:48.959> are<00:12:49.120> not<00:12:49.279> an<00:12:49.440> issue.<00:12:50.079> The
[00:12:50] The parentheses are not an issue. The
[00:12:50] The parentheses are not an issue. The syntax<00:12:50.720> is<00:12:50.959> extremely<00:12:51.440> simple.<00:12:52.399> It<00:12:52.639> is<00:12:52.800> a<00:12:53.040> very
[00:12:53] syntax is extremely simple. It is a very
[00:12:53] syntax is extremely simple. It is a very viable<00:12:53.839> language<00:12:54.320> to<00:12:55.200> support<00:12:55.440> for<00:12:55.680> plug-in
[00:12:56] viable language to support for plug-in
[00:12:56] viable language to support for plug-in development.<00:12:56.639> My<00:12:56.800> guess<00:12:56.959> is<00:12:57.120> this<00:12:57.360> got<00:12:57.440> a<00:12:57.600> ton
[00:12:57] development. My guess is this got a ton
[00:12:57] development. My guess is this got a ton of<00:12:57.839> down<00:12:58.000> votes<00:12:58.399> because<00:12:58.800> the<00:12:59.040> language<00:12:59.440> at
[00:12:59] of down votes because the language at
[00:12:59] of down votes because the language at this<00:12:59.839> point<00:13:00.000> was<00:13:00.240> already<00:13:00.480> decided<00:13:00.880> on<00:13:01.120> and
[00:13:01] this point was already decided on and
[00:13:01] this point was already decided on and this<00:13:01.360> is<00:13:01.519> over<00:13:01.680> a<00:13:01.839> year<00:13:02.000> after<00:13:02.240> the<00:13:02.399> pull
[00:13:02] this is over a year after the pull
[00:13:02] this is over a year after the pull request<00:13:03.040> was<00:13:03.680> initially<00:13:04.399> submitted.<00:13:05.360> So
[00:13:05] request was initially submitted. So
[00:13:05] request was initially submitted. So that's<00:13:05.920> that's<00:13:06.240> my<00:13:06.399> guess<00:13:06.639> there.<00:13:06.959> Another
[00:13:07] that's that's my guess there. Another
[00:13:07] that's that's my guess there. Another comment<00:13:07.760> just<00:13:08.000> curious<00:13:08.240> why<00:13:08.399> Scheme<00:13:08.720> instead
[00:13:08] comment just curious why Scheme instead
[00:13:08] comment just curious why Scheme instead of<00:13:09.040> writing<00:13:09.360> packages<00:13:09.680> in<00:13:09.920> Rust<00:13:10.240> itself.<00:13:10.959> Part
[00:13:11] of writing packages in Rust itself. Part
[00:13:11] of writing packages in Rust itself. Part of<00:13:11.200> the<00:13:11.360> reason<00:13:11.519> I<00:13:11.760> use<00:13:11.839> Helix<00:13:12.320> is<00:13:12.480> because
[00:13:12] of the reason I use Helix is because
[00:13:12] of the reason I use Helix is because everything<00:13:13.200> is<00:13:13.360> in<00:13:13.519> Rust<00:13:13.839> and<00:13:14.079> can<00:13:14.320> be<00:13:14.480> very
[00:13:14] everything is in Rust and can be very
[00:13:14] everything is in Rust and can be very performant.<00:13:16.320> Very<00:13:16.639> valid<00:13:17.040> point.<00:13:17.440> I<00:13:17.760> this
[00:13:18] performant. Very valid point. I this
[00:13:18] performant. Very valid point. I this resonates<00:13:18.480> with<00:13:18.639> me.<00:13:19.040> I<00:13:19.279> would<00:13:19.440> love<00:13:19.600> to<00:13:19.760> write
[00:13:19] resonates with me. I would love to write
[00:13:20] resonates with me. I would love to write plugins<00:13:20.320> in<00:13:20.560> Rust.<00:13:21.360> One<00:13:21.519> of<00:13:21.600> the<00:13:21.760> problems
[00:13:21] plugins in Rust. One of the problems
[00:13:22] plugins in Rust. One of the problems here<00:13:22.240> I<00:13:22.399> think<00:13:22.560> that<00:13:22.800> was<00:13:23.120> this<00:13:23.360> was<00:13:23.519> answered.
[00:13:24] here I think that was this was answered.
[00:13:24] here I think that was this was answered. One<00:13:24.320> of<00:13:24.399> the<00:13:24.560> problems<00:13:24.720> here<00:13:24.959> is<00:13:25.120> that<00:13:25.279> now<00:13:25.440> you
[00:13:25] One of the problems here is that now you
[00:13:25] One of the problems here is that now you have<00:13:25.680> to<00:13:25.839> deal<00:13:25.920> with<00:13:26.079> compilation<00:13:26.639> targets.
[00:13:27] have to deal with compilation targets.
[00:13:27] have to deal with compilation targets. You<00:13:27.600> have<00:13:27.680> to<00:13:27.839> compile<00:13:28.160> the<00:13:28.399> plugin<00:13:28.720> on<00:13:28.959> every
[00:13:29] You have to compile the plugin on every
[00:13:29] You have to compile the plugin on every system<00:13:29.360> that<00:13:29.600> it<00:13:29.680> goes<00:13:29.839> to<00:13:30.079> or<00:13:30.320> pre-ompile<00:13:30.880> it.
[00:13:31] system that it goes to or pre-ompile it.
[00:13:31] system that it goes to or pre-ompile it. So<00:13:31.360> you<00:13:31.519> have<00:13:31.600> to<00:13:31.680> have<00:13:31.760> a<00:13:31.839> pre-ompiled<00:13:32.480> plugin
[00:13:32] So you have to have a pre-ompiled plugin
[00:13:32] So you have to have a pre-ompiled plugin for<00:13:33.040> every<00:13:33.360> possible<00:13:33.839> architecture.<00:13:34.880> And<00:13:35.200> I
[00:13:35] for every possible architecture. And I
[00:13:35] for every possible architecture. And I think<00:13:35.839> the<00:13:36.160> route<00:13:36.399> for<00:13:36.800> writing<00:13:37.040> Rust<00:13:37.360> plugins
[00:13:37] think the route for writing Rust plugins
[00:13:37] think the route for writing Rust plugins is<00:13:37.920> not<00:13:38.079> completely<00:13:38.480> closed.<00:13:39.040> There<00:13:39.360> is<00:13:39.760> a<00:13:40.800> uh
[00:13:41] is not completely closed. There is a uh
[00:13:41] is not completely closed. There is a uh like<00:13:41.440> a<00:13:41.760> shared<00:13:42.079> like<00:13:42.240> a<00:13:42.399> DIL<00:13:42.959> API<00:13:43.760> that<00:13:44.000> is
[00:13:44] like a shared like a DIL API that is
[00:13:44] like a shared like a DIL API that is quite<00:13:44.560> good.<00:13:45.040> In<00:13:45.200> fact,<00:13:45.360> I<00:13:45.519> spent<00:13:45.680> a<00:13:45.920> lot<00:13:46.000> of
[00:13:46] quite good. In fact, I spent a lot of
[00:13:46] quite good. In fact, I spent a lot of time<00:13:46.240> on<00:13:46.399> it.<00:13:46.639> So<00:13:46.720> you<00:13:46.959> can<00:13:47.040> write<00:13:47.200> Rust<00:13:47.600> code,
[00:13:48] time on it. So you can write Rust code,
[00:13:48] time on it. So you can write Rust code, build<00:13:48.480> a<00:13:48.720> shared<00:13:48.959> library<00:13:49.440> and<00:13:50.000> steel<00:13:50.320> can
[00:13:50] build a shared library and steel can
[00:13:50] build a shared library and steel can pick<00:13:50.639> it<00:13:50.720> up.
[00:13:51] pick it up.
[00:13:51] pick it up. &gt;&gt; But<00:13:51.200> yeah,<00:13:51.360> no<00:13:51.600> doubt<00:13:51.760> that<00:13:51.920> this<00:13:52.079> sentiment
[00:13:52] &gt;&gt; But yeah, no doubt that this sentiment
[00:13:52] &gt;&gt; But yeah, no doubt that this sentiment is<00:13:52.639> shared<00:13:52.959> by<00:13:53.040> a<00:13:53.200> lot<00:13:53.360> of<00:13:53.440> people.
[00:13:53] is shared by a lot of people.
[00:13:53] is shared by a lot of people. &gt;&gt; I<00:13:54.160> have<00:13:54.320> been<00:13:54.399> working<00:13:54.639> on<00:13:54.800> this<00:13:55.120> for<00:13:55.360> like
[00:13:55] &gt;&gt; I have been working on this for like
[00:13:55] &gt;&gt; I have been working on this for like three<00:13:55.920> years<00:13:56.160> straight<00:13:56.800> maybe.<00:13:57.120> I<00:13:57.199> don't<00:13:57.360> know
[00:13:57] three years straight maybe. I don't know
[00:13:57] three years straight maybe. I don't know who<00:13:57.839> opened<00:13:58.079> it.<00:13:58.240> Yeah,
[00:13:58] who opened it. Yeah,
[00:13:58] who opened it. Yeah, &gt;&gt; the<00:13:58.720> the<00:13:58.880> PR<00:13:59.199> was<00:13:59.360> open<00:13:59.440> in<00:13:59.600> 2023.<00:14:00.320> I<00:14:00.959> it's
[00:14:01] &gt;&gt; the the PR was open in 2023. I it's
[00:14:01] &gt;&gt; the the PR was open in 2023. I it's early<00:14:01.920> with<00:14:02.160> respect<00:14:02.480> to<00:14:02.639> the<00:14:02.880> to<00:14:03.120> being
[00:14:03] early with respect to the to being
[00:14:03] early with respect to the to being merged<00:14:03.760> and<00:14:03.920> master,<00:14:04.480> but<00:14:04.720> it's<00:14:04.880> not<00:14:05.120> early
[00:14:05] merged and master, but it's not early
[00:14:05] merged and master, but it's not early with<00:14:05.600> respect<00:14:05.920> to<00:14:06.079> the<00:14:06.240> life<00:14:06.480> cycle<00:14:06.800> of<00:14:06.959> the
[00:14:07] with respect to the life cycle of the
[00:14:07] with respect to the life cycle of the whole<00:14:07.360> thing.<00:14:07.600> Like<00:14:08.240> I<00:14:08.560> constantly,<00:14:09.519> you
[00:14:09] whole thing. Like I constantly, you
[00:14:09] whole thing. Like I constantly, you know,<00:14:09.760> pull<00:14:09.920> in<00:14:10.320> master.<00:14:10.880> I<00:14:11.040> constantly<00:14:11.519> fix
[00:14:11] know, pull in master. I constantly fix
[00:14:11] know, pull in master. I constantly fix merge<00:14:12.160> conflicts<00:14:12.720> and<00:14:13.279> make<00:14:13.440> sure<00:14:13.680> everything
[00:14:13] merge conflicts and make sure everything
[00:14:13] merge conflicts and make sure everything is<00:14:14.160> working.<00:14:14.560> There<00:14:14.800> it<00:14:15.040> is.<00:14:16.160> I<00:14:16.639> spent
[00:14:17] is working. There it is. I spent
[00:14:17] is working. There it is. I spent weekends<00:14:17.680> documenting<00:14:18.320> every<00:14:18.639> function<00:14:19.040> like
[00:14:19] weekends documenting every function like
[00:14:19] weekends documenting every function like to<00:14:19.440> the<00:14:19.600> best<00:14:19.760> of<00:14:19.920> my<00:14:20.079> ability.<00:14:20.560> Right?<00:14:20.800> So,<00:14:20.880> it
[00:14:21] to the best of my ability. Right? So, it
[00:14:21] to the best of my ability. Right? So, it is<00:14:21.279> it's<00:14:21.600> early,<00:14:22.000> but<00:14:22.240> it's<00:14:22.399> also<00:14:22.720> not.<00:14:23.199> So,
[00:14:23] is it's early, but it's also not. So,
[00:14:23] is it's early, but it's also not. So, that's<00:14:24.240> there<00:14:24.560> there<00:14:24.800> is<00:14:25.040> quite<00:14:25.680> there's
[00:14:25] that's there there is quite there's
[00:14:26] that's there there is quite there's quite<00:14:26.160> a<00:14:26.320> bit<00:14:26.399> of<00:14:26.560> time<00:14:26.800> that<00:14:27.040> has<00:14:27.199> been<00:14:27.360> put
[00:14:27] quite a bit of time that has been put
[00:14:27] quite a bit of time that has been put and<00:14:27.760> and<00:14:28.079> care<00:14:28.320> that<00:14:28.480> has<00:14:28.639> been<00:14:28.800> put<00:14:28.959> into<00:14:29.120> it.
[00:14:29] and and care that has been put into it.
[00:14:29] and and care that has been put into it. Thank<00:14:29.600> you<00:14:29.680> so<00:14:29.839> much<00:14:30.000> to<00:14:30.160> Matt<00:14:30.399> Paris<00:14:30.800> for
[00:14:31] Thank you so much to Matt Paris for
[00:14:31] Thank you so much to Matt Paris for persevering<00:14:31.600> through<00:14:31.839> this<00:14:32.000> poll<00:14:32.320> request.<00:14:32.720> I
[00:14:32] persevering through this poll request. I
[00:14:32] persevering through this poll request. I have<00:14:33.040> not<00:14:33.199> seen<00:14:33.519> another<00:14:33.839> poll<00:14:34.079> request<00:14:34.320> that
[00:14:34] have not seen another poll request that
[00:14:34] have not seen another poll request that has<00:14:34.720> been<00:14:34.880> open<00:14:35.519> nearly<00:14:35.920> this<00:14:36.240> long.<00:14:36.720> Let<00:14:36.959> me
[00:14:37] has been open nearly this long. Let me
[00:14:37] has been open nearly this long. Let me know<00:14:37.120> in<00:14:37.279> the<00:14:37.360> comments<00:14:37.519> what<00:14:37.680> Helix<00:14:38.079> plugins
[00:14:38] know in the comments what Helix plugins
[00:14:38] know in the comments what Helix plugins you<00:14:38.639> want<00:14:38.720> to<00:14:38.880> see.<00:14:39.279> I<00:14:39.519> personally<00:14:39.839> want<00:14:39.920> to
[00:14:40] you want to see. I personally want to
[00:14:40] you want to see. I personally want to see<00:14:40.160> an<00:14:40.320> [[obsidian|Obsidian]]<00:14:40.800> plugin.<00:14:41.120> That's<00:14:41.279> like<00:14:41.440> my
[00:14:41] see an [[obsidian|Obsidian]] plugin. That's like my
[00:14:41] see an Obsidian plugin. That's like my number<00:14:41.839> one.<00:14:42.880> That's<00:14:43.120> just<00:14:43.279> me.<00:14:43.760> What<00:14:43.920> do<00:14:44.079> you
[00:14:44] number one. That's just me. What do you
[00:14:44] number one. That's just me. What do you want<00:14:44.240> to<00:14:44.399> see?<00:14:44.720> Oh,<00:14:44.880> and<00:14:45.040> if<00:14:45.120> you<00:14:45.199> want<00:14:45.279> to<00:14:45.360> know
[00:14:45] want to see? Oh, and if you want to know
[00:14:45] want to see? Oh, and if you want to know more<00:14:45.600> about<00:14:45.760> Helix<00:14:46.079> the<00:14:46.240> editor<00:14:46.560> more
[00:14:46] more about Helix the editor more
[00:14:46] more about Helix the editor more broadly,<00:14:47.120> check<00:14:47.279> out<00:14:47.360> this<00:14:47.600> video<00:14:47.760> where<00:14:47.920> I
[00:14:48] broadly, check out this video where I
[00:14:48] broadly, check out this video where I talk<00:14:48.399> about<00:14:48.639> all<00:14:48.800> the<00:14:48.959> key<00:14:49.199> bindings<00:14:49.519> and<00:14:49.760> some
[00:14:49] talk about all the key bindings and some
[00:14:49] talk about all the key bindings and some of<00:14:49.920> the<00:14:50.160> interesting<00:14:50.560> features<00:14:51.199> in<00:14:51.440> the
[00:14:51] of the interesting features in the
[00:14:51] of the interesting features in the editor<00:14:52.000> itself,<00:14:52.480> irrespective<00:14:53.040> of<00:14:53.199> plugins.
[00:14:53] editor itself, irrespective of plugins.
[00:14:53] editor itself, irrespective of plugins. Thank<00:14:53.839> you<00:14:53.920> all<00:14:54.160> for<00:14:54.320> watching<00:14:54.480> and<00:14:54.720> we'll<00:14:54.959> see
[00:14:55] Thank you all for watching and we'll see
[00:14:55] Thank you all for watching and we'll see you<00:14:55.199> in<00:14:55.360> the<00:14:55.519> next<00:14:55.680> one.

---

*Source: [https://www.youtube.com/watch?v=YDYTYktziyI](https://www.youtube.com/watch?v=YDYTYktziyI)*
